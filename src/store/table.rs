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
}
