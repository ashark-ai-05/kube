use anyhow::{Context as _, anyhow};
use kube::Client;
use kube::api::ListParams;
use kube::core::Request as KubeRequest;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableColumn {
    pub name: String,
    /// kubectl shows priority 0 always, and >0 only under `-o wide`.
    pub priority: i32,
}

/// The object a displayed row refers to, decoded from the row's
/// `PartialObjectMetadata` (present only when the fetch asked for one via
/// `includeObject=Metadata`; see `fetch_table`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowIdentity {
    pub namespace: Option<String>,
    pub name: String,
}

/// One displayed row: its cell text, bundled with the identity of the
/// object it came from.
///
/// `identity` lives ON the row rather than in a parallel `Vec` indexed
/// alongside `rows`, specifically so it is impossible to separate the two:
/// any code that reorders rows (`sort_table_rows`) moves a row's identity
/// with it automatically, by construction, rather than needing to remember
/// to reorder a second collection in lockstep. A parallel-vector design
/// would compile fine right up until someone reorders one and forgets the
/// other — the same "two collections that must agree" shape this project
/// has paid for repeatedly (see `store::watch::ResourceStore`'s own doc
/// comments on `availability`/`tables`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRow {
    pub cells: Vec<String>,
    /// `None` when the server did not return object metadata for this row
    /// — an apiserver that ignored `includeObject`, or a decode of a Table
    /// response built without it. Callers resolving a row back to an
    /// object (`row_identity`) must treat that as "cannot resolve," never
    /// fall back to guessing an identity from cell text: a CRD's NAME
    /// column is not guaranteed to literally be `metadata.name`, and a
    /// wrong guess is worse than an honest "unknown."
    pub identity: Option<RowIdentity>,
}

#[derive(Debug, Clone, Default)]
pub struct TableData {
    pub columns: Vec<TableColumn>,
    pub rows: Vec<TableRow>,
}

/// Resolve a displayed row back to the object it came from.
///
/// Selection must always go through this, never by indexing a separately
/// held list of live-watched objects with the row's position: the server
/// Table and the watch are refreshed at different moments (a fetch is a
/// point-in-time request; the watch is continuously updated) and are not
/// guaranteed to agree on row order, or even row count, at any given
/// instant. Resolving positionally against the wrong list answers "click
/// one row, inspect a different object" — the shape of bug that shipped
/// twice in Plan 1's hit-zone code, here one layer up at the data level
/// instead of the pixel level.
///
/// Returns `(namespace, name)` — `None` if `row` is out of range, or if the
/// row exists but carries no identity (see `TableRow::identity`).
pub fn row_identity(table: &TableData, row: usize) -> Option<(Option<String>, String)> {
    let identity = table.rows.get(row)?.identity.as_ref()?;
    Some((identity.namespace.clone(), identity.name.clone()))
}

/// Render one Table cell. Cells are heterogeneous — strings, integers for
/// restart counts, nulls, occasionally nested objects — and none may vanish.
pub fn cell_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Decode a `meta.k8s.io/v1 Table` response.
///
/// A CRD's declared printer columns and the rows the server returns are not
/// guaranteed to agree in length, so ragged rows are padded and over-long
/// rows truncated. Getting this wrong panics mid-render on someone's CRD.
pub fn decode_table(json: &serde_json::Value) -> anyhow::Result<TableData> {
    let defs = json
        .get("columnDefinitions")
        .and_then(|d| d.as_array())
        .ok_or_else(|| anyhow!("response has no columnDefinitions; not a Table"))?;

    let columns: Vec<TableColumn> = defs
        .iter()
        .map(|d| TableColumn {
            name: d
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string(),
            priority: d.get("priority").and_then(|p| p.as_i64()).unwrap_or(0) as i32,
        })
        .collect();

    let width = columns.len();
    let rows = json
        .get("rows")
        .and_then(|r| r.as_array())
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    let mut cells: Vec<String> = row
                        .get("cells")
                        .and_then(|c| c.as_array())
                        .map(|c| c.iter().map(cell_to_string).collect())
                        .unwrap_or_default();
                    cells.resize(width, String::new());
                    let identity = row.get("object").and_then(identity_from_object);
                    TableRow { cells, identity }
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(TableData { columns, rows })
}

/// Decode a row's `object` field (present only under `includeObject=Metadata`
/// or `includeObject=Object`) into a `RowIdentity`. `None` for anything that
/// doesn't have at least `metadata.name` — an apiserver that ignored
/// `includeObject`, or hand-built test JSON with no `object` field at all.
fn identity_from_object(object: &serde_json::Value) -> Option<RowIdentity> {
    let metadata = object.get("metadata")?;
    let name = metadata.get("name")?.as_str()?.to_string();
    let namespace = metadata
        .get("namespace")
        .and_then(|n| n.as_str())
        .map(|s| s.to_string());
    Some(RowIdentity { namespace, name })
}

/// Requested sort for the live table: which column (a 0-based index into
/// each row's cells) and which direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortState {
    pub column: usize,
    pub descending: bool,
}

/// Sort table rows by one column, in place.
///
/// Comparison is numeric when BOTH sides of a pair parse as `f64` (so
/// RESTARTS-style integer columns sort `2 < 9 < 10`, not lexically `10 <
/// 2 < 9`), and lexical otherwise — kubectl's own columns mix free text
/// (NAME, STATUS) with numbers, and there is no column-type metadata
/// available at this layer to decide up front which a given column is.
///
/// The sort is stable (`[T]::sort_by`, not `sort_unstable_by`): rows arrive
/// in the store's insertion order, and reshuffling equal keys on every
/// redraw would be visible and disorienting to a user watching the table.
///
/// A CRD's declared columns and its actual rows are not guaranteed to agree
/// in length, and a sort request can outlive a kind switch that shrinks the
/// row width — so a column index at or past ANY row's length leaves every
/// row untouched rather than panicking on an out-of-bounds index or
/// partially reordering what it could compare.
///
/// Takes `&mut [Vec<String>]` rather than `&mut Vec<Vec<String>>` (clippy's
/// `ptr_arg`, same reasoning as `store::multi::prioritise`): sorting never
/// resizes the vector, so a slice is all it needs, and every real caller
/// holding a `Vec<Vec<String>>` still calls this as `sort_rows(&mut rows,
/// ...)` unchanged via the usual deref coercion.
pub fn sort_rows(rows: &mut [Vec<String>], sort: &SortState) {
    if rows.iter().any(|row| sort.column >= row.len()) {
        return;
    }
    rows.sort_by(|a, b| {
        let ordering = compare_cells(&a[sort.column], &b[sort.column]);
        if sort.descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
}

/// The order `sort_rows` puts rows in, expressed as indices into the
/// UNSORTED list.
///
/// Selection is an index into what the user can see, and what the user can
/// see is the sorted list — but `sort_rows` works on cells alone, so once a
/// builtin-column table is sorted there is nothing left tying a displayed
/// position back to the object it came from. This returns exactly that
/// mapping, so a caller can answer "which object is on screen line N" by
/// reproducing the view's own ordering rather than approximating it (or,
/// worse, indexing the unsorted object list with a sorted row number and
/// opening a detail pane on a different object — the failure `row_identity`
/// exists to prevent on the server-columns path).
///
/// Same comparator, same stability and the same out-of-range no-op as
/// `sort_rows`, and pinned to agree with it by test: any divergence between
/// the two is the bug this function exists to prevent.
pub fn sorted_indices(rows: &[Vec<String>], sort: &SortState) -> Vec<usize> {
    let mut order: Vec<usize> = (0..rows.len()).collect();
    if rows.iter().any(|row| sort.column >= row.len()) {
        return order;
    }
    // `sort_by`, not `sort_unstable_by`, matching `sort_rows` — equal keys
    // must keep input order, or the mapping this returns disagrees with the
    // one the view drew for exactly the rows that tie.
    order.sort_by(|&a, &b| {
        let ordering = compare_cells(&rows[a][sort.column], &rows[b][sort.column]);
        if sort.descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
    order
}

/// Compare two cell values numerically if both parse as `f64`, lexically
/// otherwise.
fn compare_cells(a: &str, b: &str) -> std::cmp::Ordering {
    match (a.parse::<f64>(), b.parse::<f64>()) {
        (Ok(x), Ok(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
        _ => a.cmp(b),
    }
}

/// As `sort_rows`, but for `TableData`'s own row type: sorts the whole
/// `TableRow` (cells AND identity together) by one column of `cells`, so a
/// row's identity always moves with its cells — see `TableRow`'s doc
/// comment for why this matters. Same stability, same numeric-if-both-parse
/// comparison, same out-of-range no-op guard as `sort_rows`; kept as a
/// separate function rather than made generic over both because
/// `render_table_with_data`'s builtin-registry path never has an identity
/// to carry and has no reason to route through the `TableRow` wrapper at
/// all.
pub fn sort_table_rows(rows: &mut [TableRow], sort: &SortState) {
    if rows.iter().any(|row| sort.column >= row.cells.len()) {
        return;
    }
    rows.sort_by(|a, b| {
        let ordering = compare_cells(&a.cells[sort.column], &b.cells[sort.column]);
        if sort.descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
}

/// Ask the API server to render a resource the way kubectl does.
///
/// kube 4.2 has no Table support, so this builds the request by hand and
/// sets the Accept header itself. Verified against kube 4.2; see
/// `docs/superpowers/plan2-api-reference.md` section B4.
///
/// Also requests `includeObject=Metadata`, so each row carries the identity
/// of the object it displays (see `TableRow`/`row_identity`) rather than
/// leaving row selection to positionally match a separately-held, separately
/// refreshed list of live-watched objects — the two are not guaranteed to
/// agree on order or count at any given instant.
pub async fn fetch_table(client: &Client, resource_url: &str) -> anyhow::Result<TableData> {
    let mut req = KubeRequest::new(resource_url)
        .list(&ListParams::default())
        .context("building the list request")?;
    req.headers_mut().insert(
        http::header::ACCEPT,
        http::HeaderValue::from_static("application/json;as=Table;v=1;g=meta.k8s.io"),
    );
    // `ListParams`/`Request::list` (kube-core 4.2) has no field for this —
    // checked `kube-core-4.2.0/src/params.rs`'s `ListParams` and its
    // `populate_qp` — so it is appended to the URI `Request::list` already
    // built, the same way the Accept header above is set on the same
    // request object rather than through a builder kube-core doesn't offer.
    // `includeObject` is a real, apimachinery-documented value of
    // `meta.k8s.io/v1 TableOptions.IncludeObject` (`None` | `Object` |
    // `Metadata`); `Metadata` is the one that returns a lightweight
    // `PartialObjectMetadata` per row rather than the full object, which is
    // all `row_identity` needs.
    *req.uri_mut() = append_query_param(req.uri(), "includeObject", "Metadata")
        .context("appending includeObject to the table request")?;
    let json: serde_json::Value = client
        .request(req)
        .await
        .with_context(|| format!("requesting a Table for {resource_url}"))?;
    decode_table(&json)
}

/// Append a `key=value` query parameter to a URI that may or may not
/// already have a query string.
///
/// `Request::list` (kube-core 4.2) always starts its query string with a
/// literal trailing `?`, even with zero parameters (`ListParams::default()`
/// populates nothing) — verified by reading `kube-core-4.2.0/src/
/// request.rs`'s `list()` and pinned by this function's own tests rather
/// than assumed, so a future kube-rs upgrade that changes it fails loudly
/// here instead of silently mis-building a URI.
fn append_query_param(uri: &http::Uri, key: &str, value: &str) -> anyhow::Result<http::Uri> {
    let uri_str = uri.to_string();
    let separator = if uri_str.ends_with('?') || uri_str.is_empty() {
        ""
    } else if uri_str.contains('?') {
        "&"
    } else {
        "?"
    };
    format!("{uri_str}{separator}{key}={value}")
        .parse()
        .context("building a URI with an appended query parameter")
}

/// How long a burst of watch changes for the active kind debounces before
/// the next Table refetch.
///
/// Not tuned against a real cluster — no cluster was available while
/// building this (see the module's own README/task-report caveats on
/// `fetch_table` itself) — chosen only to smooth out the many rapid
/// `StoreChanged` events a single rollout/apply can produce into one
/// refetch rather than one per delta, which would hammer the API server on
/// a busy namespace. Task 10 should revisit this value against a real
/// cluster before relying on it.
pub const TABLE_REFETCH_DEBOUNCE: Duration = Duration::from_millis(750);

/// A ceiling on how long a Table can go un-refetched once it is known stale.
///
/// `refetch_is_due`'s debounce is trailing-edge only: it fires once a kind
/// has gone `TABLE_REFETCH_DEBOUNCE` without a further change. A kind whose
/// watch delivers MORE often than that — a rolling update of a 50-replica
/// Deployment, with Pod status transitions arriving well under 750ms apart
/// for minutes — never settles, so it never refetches at all. The status bar
/// and the sidebar's counts keep ticking (both read live objects, not the
/// Table), while `column_source` prefers `ColumnSource::Server` once any
/// fetch has landed — so the entire table BODY, not just its columns,
/// freezes at whatever snapshot was fetched before the churn started, with
/// "live" printed next to it the whole time.
///
/// This bounds the worst case: once the last successful fetch is this old
/// AND something has changed since it, a refetch is due regardless of
/// whether the debounce has settled. Not tuned against a real cluster, same
/// caveat as `TABLE_REFETCH_DEBOUNCE`.
pub const MAX_TABLE_STALENESS: Duration = Duration::from_secs(3);

/// Whether a Table refetch is due for a kind, given when the last fetch for
/// it completed (if ever) and when the watch last reported a change.
///
/// The watch is the trigger, not a poll loop: an idle kind with no changes
/// costs nothing, because nothing ever calls this for it. Fires when EITHER:
///
/// - **Something changed since the last fetch, and the change has
///   settled.** `last_fetch` predating `last_change` (or not existing at
///   all) means the fetch on hand is stale; `now` must then be at least
///   `debounce` past `last_change`, so a burst of deltas (a rollout
///   touching fifty pods) collapses into one refetch after the burst
///   quiets down, not one per delta. A `last_fetch` at or after
///   `last_change` means it already reflects everything observed so far,
///   so refetching would just repeat the same request — this is what keeps
///   a fairly static namespace from being refetched merely because time
///   passed.
/// - **The last fetch is older than `MAX_TABLE_STALENESS`, and something
///   has changed since it.** This is the ceiling: a kind changing faster
///   than `debounce` never satisfies the settle condition above, so without
///   this it would never refetch at all — see `MAX_TABLE_STALENESS`'s own
///   doc comment.
///
/// `now` is a parameter rather than read from the clock internally so this
/// stays a pure function callers can test against fixed instants.
pub fn refetch_is_due(
    last_fetch: Option<Instant>,
    last_change: Instant,
    now: Instant,
    debounce: Duration,
) -> bool {
    let fetch_is_stale = match last_fetch {
        None => true,
        Some(t) => t < last_change,
    };
    if !fetch_is_stale {
        return false;
    }
    let settled = now.saturating_duration_since(last_change) >= debounce;
    let ceiling_exceeded = last_fetch
        .map(|t| now.saturating_duration_since(t) >= MAX_TABLE_STALENESS)
        .unwrap_or(false);
    settled || ceiling_exceeded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> serde_json::Value {
        serde_json::json!({
            "kind": "Table",
            "columnDefinitions": [
                {"name": "Name", "type": "string", "priority": 0},
                {"name": "Ready", "type": "string", "priority": 0},
                {"name": "Status", "type": "string", "priority": 0},
                {"name": "IP", "type": "string", "priority": 1}
            ],
            "rows": [
                {"cells": ["api-x2k", "2/2", "Running", "10.244.1.37"]},
                {"cells": ["api-q8w", "1/2", "CrashLoopBackOff", "10.244.1.38"]}
            ]
        })
    }

    // --- Task 10: mapping a displayed row back to its object ---

    /// Rows whose sort order differs from their input order in every way a
    /// wrong `sorted_indices` could get right by accident: the numeric column
    /// (index 1) sorts `2 < 9 < 10` numerically but `10 < 2 < 9` lexically,
    /// two rows tie on it so stability is observable, and no row is already
    /// in its final position.
    fn permuting_rows() -> Vec<Vec<String>> {
        [
            ["delta", "10", "x"],
            ["alpha", "2", "y"],
            ["charlie", "2", "z"],
            ["bravo", "9", "w"],
        ]
        .iter()
        .map(|r| r.iter().map(|s| s.to_string()).collect())
        .collect()
    }

    #[test]
    fn sorted_indices_reproduce_exactly_what_sort_rows_produces() {
        // The whole point: a caller uses this to map a screen position back
        // to an object, so any divergence from the ordering the view actually
        // draws resolves the click to the wrong object. Checked in both
        // directions, on a fixture where numeric-vs-lexical and stability
        // both change the answer.
        for descending in [false, true] {
            for column in 0..3 {
                let sort = SortState { column, descending };
                let mut expected = permuting_rows();
                sort_rows(&mut expected, &sort);

                let original = permuting_rows();
                let order = sorted_indices(&original, &sort);
                let got: Vec<Vec<String>> = order.iter().map(|&i| original[i].clone()).collect();
                assert_eq!(
                    got, expected,
                    "sorted_indices disagreed with sort_rows for {sort:?}"
                );
                assert_eq!(order.len(), original.len(), "every row must be placed");
            }
        }
    }

    #[test]
    fn sorted_indices_are_a_permutation_that_keeps_ties_in_input_order() {
        // "alpha" and "charlie" both hold "2". A stable sort keeps them in
        // input order (indices 1 then 2); `sort_unstable_by` is free not to,
        // and a row that swaps under the user between frames is exactly the
        // disorientation `sort_rows` documents avoiding.
        let order = sorted_indices(
            &permuting_rows(),
            &SortState {
                column: 1,
                descending: false,
            },
        );
        assert_eq!(
            order,
            vec![1, 2, 3, 0],
            "expected 2(alpha), 2(charlie), 9(bravo), 10(delta) — numeric, \
             ties in input order"
        );
    }

    #[test]
    fn sorted_indices_for_an_out_of_range_column_leave_the_order_untouched() {
        // Matches `sort_rows`' own guard: a sort request can outlive a kind
        // switch that shrinks the row width.
        let rows = permuting_rows();
        let order = sorted_indices(
            &rows,
            &SortState {
                column: 9,
                descending: false,
            },
        );
        assert_eq!(order, vec![0, 1, 2, 3]);
    }

    #[test]
    fn decodes_columns_in_order() {
        let t = decode_table(&sample()).unwrap();
        let names: Vec<&str> = t.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["Name", "Ready", "Status", "IP"]);
    }

    #[test]
    fn decodes_rows_as_strings() {
        let t = decode_table(&sample()).unwrap();
        assert_eq!(t.rows.len(), 2);
        assert_eq!(
            t.rows[0].cells,
            vec!["api-x2k", "2/2", "Running", "10.244.1.37"]
        );
    }

    #[test]
    fn retains_column_priority_for_narrow_terminals() {
        let t = decode_table(&sample()).unwrap();
        assert_eq!(
            t.columns[3].priority, 1,
            "priority>0 columns are kubectl's -o wide extras"
        );
    }

    #[test]
    fn a_missing_priority_defaults_to_zero() {
        let json = serde_json::json!({
            "columnDefinitions": [{"name": "Name", "type": "string"}],
            "rows": []
        });
        assert_eq!(decode_table(&json).unwrap().columns[0].priority, 0);
    }

    #[test]
    fn non_string_cells_are_rendered_not_dropped() {
        // Cells are heterogeneous: integers for restart counts, nulls, and
        // occasionally nested objects. None of them may vanish.
        assert_eq!(cell_to_string(&serde_json::json!("x")), "x");
        assert_eq!(cell_to_string(&serde_json::json!(7)), "7");
        assert_eq!(cell_to_string(&serde_json::json!(true)), "true");
        assert_eq!(cell_to_string(&serde_json::json!(null)), "");
        assert_eq!(cell_to_string(&serde_json::json!({"a": 1})), "{\"a\":1}");
    }

    #[test]
    fn a_row_with_fewer_cells_than_columns_is_padded_not_panicked() {
        let json = serde_json::json!({
            "columnDefinitions": [{"name": "A"}, {"name": "B"}, {"name": "C"}],
            "rows": [{"cells": ["only-one"]}]
        });
        let t = decode_table(&json).unwrap();
        assert_eq!(
            t.rows[0].cells.len(),
            3,
            "ragged rows must be padded to the column count"
        );
        assert_eq!(t.rows[0].cells[0], "only-one");
        assert_eq!(t.rows[0].cells[2], "");
    }

    #[test]
    fn a_row_with_more_cells_than_columns_is_truncated() {
        let json = serde_json::json!({
            "columnDefinitions": [{"name": "A"}],
            "rows": [{"cells": ["a", "b", "c"]}]
        });
        assert_eq!(decode_table(&json).unwrap().rows[0].cells.len(), 1);
    }

    #[test]
    fn malformed_json_is_an_error_not_a_panic() {
        assert!(decode_table(&serde_json::json!({"nope": true})).is_err());
        assert!(decode_table(&serde_json::json!([])).is_err());
    }

    // --- row identity (includeObject=Metadata) ---

    #[test]
    fn a_row_with_no_object_field_decodes_no_identity() {
        // The un-augmented Table shape this project's other fixtures already
        // used before includeObject existed — must not error, must not guess.
        let t = decode_table(&sample()).unwrap();
        assert!(t.rows.iter().all(|r| r.identity.is_none()));
    }

    #[test]
    fn a_row_with_object_metadata_decodes_its_identity() {
        let json = serde_json::json!({
            "columnDefinitions": [{"name": "Name"}],
            "rows": [{
                "cells": ["api-x2k"],
                "object": {
                    "kind": "PartialObjectMetadata",
                    "metadata": {"name": "api-x2k", "namespace": "default"}
                }
            }]
        });
        let t = decode_table(&json).unwrap();
        assert_eq!(
            t.rows[0].identity,
            Some(RowIdentity {
                namespace: Some("default".to_string()),
                name: "api-x2k".to_string(),
            })
        );
    }

    #[test]
    fn a_cluster_scoped_objects_identity_has_no_namespace() {
        let json = serde_json::json!({
            "columnDefinitions": [{"name": "Name"}],
            "rows": [{
                "cells": ["node-1"],
                "object": {"metadata": {"name": "node-1"}}
            }]
        });
        let t = decode_table(&json).unwrap();
        assert_eq!(
            t.rows[0].identity,
            Some(RowIdentity {
                namespace: None,
                name: "node-1".to_string(),
            })
        );
    }

    #[test]
    fn object_metadata_with_no_name_decodes_no_identity_rather_than_guessing() {
        let json = serde_json::json!({
            "columnDefinitions": [{"name": "Name"}],
            "rows": [{"cells": ["x"], "object": {"metadata": {}}}]
        });
        assert_eq!(decode_table(&json).unwrap().rows[0].identity, None);
    }

    #[test]
    fn a_row_carries_the_identity_of_the_object_it_displays() {
        let table = TableData {
            columns: vec![],
            rows: vec![TableRow {
                cells: vec!["x".to_string()],
                identity: Some(RowIdentity {
                    namespace: Some("default".to_string()),
                    name: "pod-x".to_string(),
                }),
            }],
        };
        assert_eq!(
            row_identity(&table, 0),
            Some((Some("default".to_string()), "pod-x".to_string()))
        );
    }

    #[test]
    fn identity_survives_a_row_order_that_differs_from_the_objects_list() {
        // The whole point: the server returned rows in one order (b, a); a
        // caller's separately held `objects` list could easily be in another
        // order (a, b). Resolving row 0 must give the object actually
        // DISPLAYED there ("pod-b"), never objects[0] ("pod-a").
        let table = TableData {
            columns: vec![],
            rows: vec![
                TableRow {
                    cells: vec!["b".to_string()],
                    identity: Some(RowIdentity {
                        namespace: None,
                        name: "pod-b".to_string(),
                    }),
                },
                TableRow {
                    cells: vec!["a".to_string()],
                    identity: Some(RowIdentity {
                        namespace: None,
                        name: "pod-a".to_string(),
                    }),
                },
            ],
        };
        assert_eq!(
            row_identity(&table, 0).map(|(_, name)| name),
            Some("pod-b".to_string())
        );
        assert_eq!(
            row_identity(&table, 1).map(|(_, name)| name),
            Some("pod-a".to_string())
        );
    }

    #[test]
    fn a_row_with_no_object_metadata_resolves_to_none_rather_than_guessing() {
        // If includeObject was not honoured by the apiserver, we must not
        // silently fall back to positional matching — that is the exact bug
        // this whole mechanism exists to prevent.
        let table = TableData {
            columns: vec![],
            rows: vec![TableRow {
                cells: vec!["x".to_string()],
                identity: None,
            }],
        };
        assert_eq!(row_identity(&table, 0), None);
    }

    #[test]
    fn resolving_a_row_past_the_end_is_none_rather_than_panicking() {
        let table = TableData::default();
        assert_eq!(row_identity(&table, 0), None);
    }

    // --- sort_table_rows: identity moves with its cells ---

    #[test]
    fn sorting_table_rows_moves_identity_with_its_cells() {
        let mut rows = vec![
            TableRow {
                cells: vec!["b".to_string()],
                identity: Some(RowIdentity {
                    namespace: None,
                    name: "pod-b".to_string(),
                }),
            },
            TableRow {
                cells: vec!["a".to_string()],
                identity: Some(RowIdentity {
                    namespace: None,
                    name: "pod-a".to_string(),
                }),
            },
        ];
        sort_table_rows(
            &mut rows,
            &SortState {
                column: 0,
                descending: false,
            },
        );
        assert_eq!(
            rows[0].identity.as_ref().map(|i| i.name.as_str()),
            Some("pod-a"),
            "the object displayed first after sorting must be the one whose \
             cells sorted first, not whichever row started first"
        );
    }

    #[test]
    fn sort_table_rows_by_a_column_beyond_the_row_width_leaves_rows_alone() {
        let mut rows = vec![TableRow {
            cells: vec!["a".to_string()],
            identity: None,
        }];
        let before = rows.clone();
        sort_table_rows(
            &mut rows,
            &SortState {
                column: 9,
                descending: false,
            },
        );
        assert_eq!(rows, before);
    }

    // --- includeObject wiring: URI manipulation ---

    #[test]
    fn kube_cores_list_request_ends_in_a_bare_question_mark_with_default_params() {
        // Pins the exact kube-core 4.2 behaviour `append_query_param` relies
        // on, rather than assuming it: a future kube-rs upgrade that changes
        // this fails loudly here instead of silently mis-building a URI.
        let req = KubeRequest::new("/api/v1/pods")
            .list(&ListParams::default())
            .unwrap();
        assert_eq!(req.uri().to_string(), "/api/v1/pods?");
    }

    #[test]
    fn append_query_param_on_a_bare_question_mark_appends_directly() {
        let uri: http::Uri = "/api/v1/pods?".parse().unwrap();
        let out = append_query_param(&uri, "includeObject", "Metadata").unwrap();
        assert_eq!(out.to_string(), "/api/v1/pods?includeObject=Metadata");
    }

    #[test]
    fn append_query_param_after_existing_params_uses_an_ampersand() {
        let uri: http::Uri = "/api/v1/pods?labelSelector=a".parse().unwrap();
        let out = append_query_param(&uri, "includeObject", "Metadata").unwrap();
        assert_eq!(
            out.to_string(),
            "/api/v1/pods?labelSelector=a&includeObject=Metadata"
        );
    }

    // --- refetch_is_due ---

    fn instant_plus(base: Instant, millis: u64) -> Instant {
        base + Duration::from_millis(millis)
    }

    #[test]
    fn no_previous_fetch_is_due_once_the_change_has_settled() {
        let base = Instant::now();
        let last_change = base;
        let now = instant_plus(base, 1000);
        assert!(refetch_is_due(
            None,
            last_change,
            now,
            Duration::from_millis(750)
        ));
    }

    #[test]
    fn no_previous_fetch_but_the_change_is_still_inside_the_debounce_window() {
        let base = Instant::now();
        let last_change = base;
        let now = instant_plus(base, 200);
        assert!(!refetch_is_due(
            None,
            last_change,
            now,
            Duration::from_millis(750)
        ));
    }

    #[test]
    fn a_change_exactly_at_the_debounce_boundary_is_due() {
        let base = Instant::now();
        let last_change = base;
        let now = instant_plus(base, 750);
        assert!(refetch_is_due(
            None,
            last_change,
            now,
            Duration::from_millis(750)
        ));
    }

    #[test]
    fn a_fetch_already_reflecting_the_latest_change_is_not_due_again() {
        // "Fairly static namespaces": nothing changed since the last
        // successful fetch, so refetching again — no matter how much time
        // passes — would just repeat the same request.
        let base = Instant::now();
        let last_change = base;
        let last_fetch = instant_plus(base, 100); // AFTER the change
        let now = instant_plus(base, 10_000);
        assert!(!refetch_is_due(
            Some(last_fetch),
            last_change,
            now,
            Duration::from_millis(750)
        ));
    }

    #[test]
    fn a_new_change_after_a_stale_fetch_is_due_once_settled() {
        let base = Instant::now();
        let last_fetch = base; // BEFORE the change
        let last_change = instant_plus(base, 50);
        let now = instant_plus(base, 50 + 750);
        assert!(refetch_is_due(
            Some(last_fetch),
            last_change,
            now,
            Duration::from_millis(750)
        ));
    }

    #[test]
    fn a_new_change_after_a_stale_fetch_but_not_yet_settled_is_not_due() {
        let base = Instant::now();
        let last_fetch = base;
        let last_change = instant_plus(base, 50);
        let now = instant_plus(base, 50 + 200);
        assert!(!refetch_is_due(
            Some(last_fetch),
            last_change,
            now,
            Duration::from_millis(750)
        ));
    }

    #[test]
    fn sustained_churn_faster_than_the_debounce_still_refetches_via_the_ceiling() {
        // A rolling update: something changes every 100ms, forever — never
        // settling for a full 750ms debounce window. Without
        // `MAX_TABLE_STALENESS` this never fires: `now` is checked exactly
        // when each change lands, so `now - last_change` is always ~0 and
        // the settle condition never holds. Simulated for 5 real seconds of
        // churn (50 ticks) so this fails loudly if the ceiling is ever
        // widened past that.
        let base = Instant::now();
        let mut last_fetch: Option<Instant> = Some(base);
        let mut fired = false;
        for tick in 1..=50u64 {
            let now = instant_plus(base, tick * 100);
            let last_change = now; // a fresh change lands right as we check
            if refetch_is_due(last_fetch, last_change, now, Duration::from_millis(750)) {
                fired = true;
                last_fetch = Some(now);
            }
        }
        assert!(
            fired,
            "a kind changing every 100ms for 5 seconds must eventually \
             refetch — otherwise the table body freezes for the entire \
             rollout while the status bar keeps reporting 'live'"
        );
    }

    #[test]
    fn the_ceiling_does_not_fire_early_for_a_fast_but_short_burst() {
        // A burst that only lasts a second or two, well under the 3s
        // ceiling, must still wait for the ordinary debounce — the ceiling
        // is a backstop for SUSTAINED churn, not a second, shorter debounce.
        let base = Instant::now();
        let last_fetch = Some(base);
        let last_change = instant_plus(base, 1_500); // 1.5s of churn so far
        let now = last_change; // checked immediately, debounce not settled
        assert!(!refetch_is_due(
            last_fetch,
            last_change,
            now,
            Duration::from_millis(750)
        ));
    }

    // --- sort_rows ---

    #[test]
    fn sorting_is_stable_and_reversible() {
        let mut rows = vec![
            vec!["b".into(), "2".into()],
            vec!["a".into(), "1".into()],
            vec!["c".into(), "2".into()],
        ];
        sort_rows(
            &mut rows,
            &SortState {
                column: 1,
                descending: false,
            },
        );
        assert_eq!(rows[0][0], "a");
        assert_eq!(
            &[rows[1][0].as_str(), rows[2][0].as_str()],
            &["b", "c"],
            "equal keys must keep their original order"
        );
    }

    #[test]
    fn sorting_is_stable_across_many_equal_keys_not_just_a_three_row_fixture() {
        // The 3-row fixture above (one repeated key among 3 rows) does not
        // actually discriminate a stable sort from an unstable one: Rust's
        // `sort_unstable_by` falls back to an insertion-sort-like pass on
        // slices this small, which happens to preserve order for exactly
        // this input — confirmed empirically before writing this test, by
        // running the same comparator through `sort_unstable_by` and
        // observing it still passed. Interleaving a few distinct keys among
        // a long run of equal ones forces genuine partition/swap work, the
        // same fixture shape `store::multi::prioritise`'s own stability
        // test needed for the identical reason.
        let mut rows: Vec<Vec<String>> = (0..40)
            .map(|i| vec![format!("row-{i:02}"), "2".to_string()])
            .collect();
        rows.insert(5, vec!["low-a".to_string(), "1".to_string()]);
        rows.insert(15, vec!["high".to_string(), "3".to_string()]);
        rows.insert(25, vec!["low-b".to_string(), "1".to_string()]);

        let expected_order: Vec<String> = rows
            .iter()
            .filter(|r| r[1] == "2")
            .map(|r| r[0].clone())
            .collect();

        sort_rows(
            &mut rows,
            &SortState {
                column: 1,
                descending: false,
            },
        );

        let actual_order: Vec<String> = rows
            .iter()
            .filter(|r| r[1] == "2")
            .map(|r| r[0].clone())
            .collect();
        assert_eq!(
            actual_order, expected_order,
            "rows sharing a sort key must keep their original relative order"
        );
    }

    #[test]
    fn sorting_descending_reverses_the_order() {
        let mut rows = vec![vec!["a".into()], vec!["c".into()], vec!["b".into()]];
        sort_rows(
            &mut rows,
            &SortState {
                column: 0,
                descending: true,
            },
        );
        assert_eq!(
            rows.iter().map(|r| r[0].as_str()).collect::<Vec<_>>(),
            vec!["c", "b", "a"]
        );
    }

    #[test]
    fn sorting_by_a_column_beyond_the_row_width_leaves_the_rows_alone() {
        let mut rows = vec![vec!["a".into()], vec!["b".into()]];
        let before = rows.clone();
        sort_rows(
            &mut rows,
            &SortState {
                column: 9,
                descending: false,
            },
        );
        assert_eq!(
            rows, before,
            "a ragged CRD table must not panic or scramble"
        );
    }

    #[test]
    fn ages_and_counts_sort_numerically_not_lexically() {
        // "10" before "9" is the classic wrong answer, and RESTARTS is one of
        // the columns people actually sort by.
        let mut rows = vec![vec!["9".into()], vec!["10".into()], vec!["2".into()]];
        sort_rows(
            &mut rows,
            &SortState {
                column: 0,
                descending: false,
            },
        );
        assert_eq!(
            rows.iter().map(|r| r[0].as_str()).collect::<Vec<_>>(),
            vec!["2", "9", "10"]
        );
    }
}
