use anyhow::{Context as _, anyhow};
use kube::Client;
use kube::api::ListParams;
use kube::core::Request as KubeRequest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableColumn {
    pub name: String,
    /// kubectl shows priority 0 always, and >0 only under `-o wide`.
    pub priority: i32,
}

#[derive(Debug, Clone, Default)]
pub struct TableData {
    pub columns: Vec<TableColumn>,
    pub rows: Vec<Vec<String>>,
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
                    cells
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(TableData { columns, rows })
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

/// Compare two cell values numerically if both parse as `f64`, lexically
/// otherwise.
fn compare_cells(a: &str, b: &str) -> std::cmp::Ordering {
    match (a.parse::<f64>(), b.parse::<f64>()) {
        (Ok(x), Ok(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
        _ => a.cmp(b),
    }
}

/// Ask the API server to render a resource the way kubectl does.
///
/// kube 4.2 has no Table support, so this builds the request by hand and
/// sets the Accept header itself. Verified against kube 4.2; see
/// `docs/superpowers/plan2-api-reference.md` section B4.
pub async fn fetch_table(client: &Client, resource_url: &str) -> anyhow::Result<TableData> {
    let mut req = KubeRequest::new(resource_url)
        .list(&ListParams::default())
        .context("building the list request")?;
    req.headers_mut().insert(
        http::header::ACCEPT,
        http::HeaderValue::from_static("application/json;as=Table;v=1;g=meta.k8s.io"),
    );
    let json: serde_json::Value = client
        .request(req)
        .await
        .with_context(|| format!("requesting a Table for {resource_url}"))?;
    decode_table(&json)
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
        assert_eq!(t.rows[0], vec!["api-x2k", "2/2", "Running", "10.244.1.37"]);
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
            t.rows[0].len(),
            3,
            "ragged rows must be padded to the column count"
        );
        assert_eq!(t.rows[0][0], "only-one");
        assert_eq!(t.rows[0][2], "");
    }

    #[test]
    fn a_row_with_more_cells_than_columns_is_truncated() {
        let json = serde_json::json!({
            "columnDefinitions": [{"name": "A"}],
            "rows": [{"cells": ["a", "b", "c"]}]
        });
        assert_eq!(decode_table(&json).unwrap().rows[0].len(), 1);
    }

    #[test]
    fn malformed_json_is_an_error_not_a_panic() {
        assert!(decode_table(&serde_json::json!({"nope": true})).is_err());
        assert!(decode_table(&serde_json::json!([])).is_err());
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
