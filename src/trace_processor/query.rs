//! Ergonomic query-result types. Walks the packed columnar `CellsBatch`
//! encoding into `Row` / `Cell` values.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, anyhow, bail};

use crate::trace_processor::proto::{QueryResult as ProtoQueryResult, query_result};

/// Fully-materialized query result. Rows are eagerly decoded since the legacy
/// HTTP endpoint sends them all before returning.
#[derive(Debug, Clone)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Row>,
    /// Wall time reported by the server, if present.
    pub elapsed_ms: Option<f64>,
}

impl QueryResult {
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Row> {
        self.rows.iter()
    }
}

/// One row of a query result. Column values are aligned with
/// [`QueryResult::columns`].
#[derive(Debug, Clone)]
pub struct Row {
    cells: Vec<Cell>,
    index: Arc<HashMap<String, usize>>,
}

impl Row {
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    pub fn get(&self, name: &str) -> Result<&Cell> {
        let i = self
            .index
            .get(name)
            .copied()
            .ok_or_else(|| anyhow!("no column named `{name}`"))?;
        Ok(&self.cells[i])
    }

    pub fn get_idx(&self, i: usize) -> Option<&Cell> {
        self.cells.get(i)
    }

    /// Test-only constructor so higher-level modules (summary, REPL, …)
    /// can build `Row` fixtures without routing through the decoder.
    /// The `index` map is left empty — `get_idx` still works, but `get`
    /// by name will fail; tests should use `cells()` or `get_idx`.
    #[cfg(test)]
    pub(crate) fn new_for_test(cells: Vec<Cell>) -> Self {
        Self {
            cells,
            index: Arc::new(HashMap::new()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Null,
    Int(i64),
    Float(f64),
    String(String),
    Blob(Vec<u8>),
}

impl Cell {
    pub fn is_null(&self) -> bool {
        matches!(self, Cell::Null)
    }

    pub fn as_int(&self) -> Result<i64> {
        match self {
            Cell::Int(v) => Ok(*v),
            other => bail!("expected int, got {other:?}"),
        }
    }

    pub fn as_float(&self) -> Result<f64> {
        match self {
            Cell::Float(v) => Ok(*v),
            Cell::Int(v) => Ok(*v as f64),
            other => bail!("expected float, got {other:?}"),
        }
    }

    pub fn as_str(&self) -> Result<&str> {
        match self {
            Cell::String(s) => Ok(s.as_str()),
            other => bail!("expected string, got {other:?}"),
        }
    }

    pub fn as_blob(&self) -> Result<&[u8]> {
        match self {
            Cell::Blob(b) => Ok(b.as_slice()),
            other => bail!("expected blob, got {other:?}"),
        }
    }

    pub fn as_int_opt(&self) -> Option<i64> {
        if let Cell::Int(v) = self { Some(*v) } else { None }
    }

    pub fn as_str_opt(&self) -> Option<&str> {
        if let Cell::String(s) = self { Some(s.as_str()) } else { None }
    }
}

/// Decode a raw [`ProtoQueryResult`] into the ergonomic form. Concatenated
/// wire-level `QueryResult` messages were already merged by
/// `http::decode_concat`, so all batches live inside `raw.batch` in order.
///
/// Walk each batch's `cells: Vec<i32>` (the packed `CellType` tags). For each
/// tag, pop the next value from the matching typed array:
/// - `CellVarint`  -> `varint_cells[vi]`
/// - `CellFloat64` -> `float64_cells[fi]`
/// - `CellString`  -> next NUL-terminated span of `string_cells`
/// - `CellBlob`    -> `blob_cells[bi]`
/// - `CellNull`    -> no array advance
/// Emit a `Row` every `columns.len()` cells.
pub(crate) fn decode(raw: ProtoQueryResult) -> Result<QueryResult> {
    let columns = raw.column_names;
    if let Some(err) = raw.error.as_deref().filter(|e| !e.is_empty()) {
        bail!("query result error: {err}");
    }
    let ncols = columns.len();

    // Pre-split string buffers by batch so each batch keeps its own NUL cursor.
    let mut rows = Vec::new();
    let mut index_map = HashMap::with_capacity(columns.len());
    for (i, name) in columns.iter().enumerate() {
        index_map.insert(name.clone(), i);
    }
    let index = Arc::new(index_map);

    let mut pending: Vec<Cell> = Vec::with_capacity(ncols);

    for batch in raw.batch {
        decode_batch(&batch, ncols, &mut pending, &mut rows, &index)?;
    }

    if !pending.is_empty() {
        bail!(
            "query result ended mid-row: {} cells pending, expected multiple of {ncols}",
            pending.len()
        );
    }

    Ok(QueryResult {
        columns,
        rows,
        elapsed_ms: raw.elapsed_time_ms,
    })
}

fn decode_batch(
    batch: &query_result::CellsBatch,
    ncols: usize,
    pending: &mut Vec<Cell>,
    rows: &mut Vec<Row>,
    index: &Arc<HashMap<String, usize>>,
) -> Result<()> {
    use query_result::cells_batch::CellType;

    if ncols == 0 {
        // No columns: nothing to emit regardless of cell tags.
        return Ok(());
    }

    let string_spans = split_string_cells(batch.string_cells.as_deref().unwrap_or(""));

    let mut vi = 0usize;
    let mut fi = 0usize;
    let mut si = 0usize;
    let mut bi = 0usize;

    for &tag_i32 in &batch.cells {
        let tag = CellType::try_from(tag_i32)
            .map_err(|_| anyhow!("unknown CellType tag {tag_i32} in QueryResult"))?;
        let cell = match tag {
            CellType::CellNull => Cell::Null,
            CellType::CellVarint => {
                let v = *batch
                    .varint_cells
                    .get(vi)
                    .ok_or_else(|| anyhow!("varint_cells exhausted at index {vi}"))?;
                vi += 1;
                Cell::Int(v)
            }
            CellType::CellFloat64 => {
                let v = *batch
                    .float64_cells
                    .get(fi)
                    .ok_or_else(|| anyhow!("float64_cells exhausted at index {fi}"))?;
                fi += 1;
                Cell::Float(v)
            }
            CellType::CellString => {
                let s = string_spans
                    .get(si)
                    .ok_or_else(|| anyhow!("string_cells exhausted at index {si}"))?;
                si += 1;
                Cell::String((*s).to_string())
            }
            CellType::CellBlob => {
                let b = batch
                    .blob_cells
                    .get(bi)
                    .ok_or_else(|| anyhow!("blob_cells exhausted at index {bi}"))?;
                bi += 1;
                Cell::Blob(b.clone())
            }
            CellType::CellInvalid => {
                bail!("QueryResult contained CELL_INVALID");
            }
        };

        pending.push(cell);
        if pending.len() == ncols {
            let cells = std::mem::take(pending);
            pending.reserve(ncols);
            rows.push(Row {
                cells,
                index: index.clone(),
            });
        }
    }

    Ok(())
}

/// The string payload is a single NUL-separated buffer. Each cell is
/// NUL-terminated so for N string cells there are N NUL separators; the trailing
/// empty span after the last NUL is ignored.
fn split_string_cells(raw: &str) -> Vec<&str> {
    if raw.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<&str> = raw.split('\0').collect();
    // Drop the trailing empty token created by the terminating NUL of the last
    // real cell. Be defensive if the server ever omits it.
    if matches!(out.last(), Some(last) if last.is_empty()) {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace_processor::proto::query_result;
    use crate::trace_processor::proto::query_result::cells_batch::CellType;

    fn cell_tag(t: CellType) -> i32 {
        t as i32
    }

    fn make_batch(
        cells: Vec<CellType>,
        varints: Vec<i64>,
        floats: Vec<f64>,
        strings: Option<&str>,
        blobs: Vec<Vec<u8>>,
        is_last: bool,
    ) -> query_result::CellsBatch {
        query_result::CellsBatch {
            cells: cells.into_iter().map(cell_tag).collect(),
            varint_cells: varints,
            float64_cells: floats,
            blob_cells: blobs,
            string_cells: strings.map(|s| s.to_string()),
            is_last_batch: Some(is_last),
        }
    }

    #[test]
    fn decodes_all_cell_kinds_and_nulls() {
        // 5 columns: int, float, str, blob, null — one row.
        let batch = make_batch(
            vec![
                CellType::CellVarint,
                CellType::CellFloat64,
                CellType::CellString,
                CellType::CellBlob,
                CellType::CellNull,
            ],
            vec![42],
            vec![3.14],
            Some("hello\0"),
            vec![vec![0xde, 0xad, 0xbe, 0xef]],
            true,
        );
        let raw = ProtoQueryResult {
            column_names: vec!["a".into(), "b".into(), "c".into(), "d".into(), "e".into()],
            error: None,
            batch: vec![batch],
            statement_count: None,
            statement_with_output_count: None,
            last_statement_sql: None,
            elapsed_time_ms: Some(1.5),
        };
        let out = decode(raw).unwrap();
        assert_eq!(out.columns, vec!["a", "b", "c", "d", "e"]);
        assert_eq!(out.rows.len(), 1);
        assert_eq!(out.elapsed_ms, Some(1.5));
        let row = &out.rows[0];
        assert_eq!(row.get("a").unwrap().as_int().unwrap(), 42);
        assert!((row.get("b").unwrap().as_float().unwrap() - 3.14).abs() < 1e-9);
        assert_eq!(row.get("c").unwrap().as_str().unwrap(), "hello");
        assert_eq!(row.get("d").unwrap().as_blob().unwrap(), &[0xde, 0xad, 0xbe, 0xef]);
        assert!(row.get("e").unwrap().is_null());
    }

    #[test]
    fn multi_row_single_batch() {
        // 2 columns x 3 rows.
        let batch = make_batch(
            vec![
                CellType::CellVarint,
                CellType::CellString,
                CellType::CellVarint,
                CellType::CellString,
                CellType::CellVarint,
                CellType::CellString,
            ],
            vec![1, 2, 3],
            vec![],
            Some("one\0two\0three\0"),
            vec![],
            true,
        );
        let raw = ProtoQueryResult {
            column_names: vec!["id".into(), "name".into()],
            error: None,
            batch: vec![batch],
            statement_count: None,
            statement_with_output_count: None,
            last_statement_sql: None,
            elapsed_time_ms: None,
        };
        let out = decode(raw).unwrap();
        assert_eq!(out.rows.len(), 3);
        assert_eq!(out.rows[0].get("id").unwrap().as_int().unwrap(), 1);
        assert_eq!(out.rows[0].get("name").unwrap().as_str().unwrap(), "one");
        assert_eq!(out.rows[1].get("name").unwrap().as_str().unwrap(), "two");
        assert_eq!(out.rows[2].get("id").unwrap().as_int().unwrap(), 3);
    }

    #[test]
    fn rows_span_multiple_batches() {
        // Row boundary falls in the middle of a batch: 2 cols, row 1 in batch 1
        // (cells = [VARINT]), row 1 completes in batch 2 (cells = [STRING]).
        let batch1 = make_batch(
            vec![CellType::CellVarint],
            vec![7],
            vec![],
            None,
            vec![],
            false,
        );
        let batch2 = make_batch(
            vec![CellType::CellString],
            vec![],
            vec![],
            Some("hi\0"),
            vec![],
            true,
        );
        let raw = ProtoQueryResult {
            column_names: vec!["a".into(), "b".into()],
            error: None,
            batch: vec![batch1, batch2],
            statement_count: None,
            statement_with_output_count: None,
            last_statement_sql: None,
            elapsed_time_ms: None,
        };
        let out = decode(raw).unwrap();
        assert_eq!(out.rows.len(), 1);
        assert_eq!(out.rows[0].get("a").unwrap().as_int().unwrap(), 7);
        assert_eq!(out.rows[0].get("b").unwrap().as_str().unwrap(), "hi");
    }

    #[test]
    fn empty_result() {
        let raw = ProtoQueryResult {
            column_names: vec!["x".into()],
            error: None,
            batch: vec![],
            statement_count: None,
            statement_with_output_count: None,
            last_statement_sql: None,
            elapsed_time_ms: None,
        };
        let out = decode(raw).unwrap();
        assert!(out.is_empty());
        assert_eq!(out.columns, vec!["x"]);
    }

    #[test]
    fn error_field_surfaces() {
        let raw = ProtoQueryResult {
            column_names: vec![],
            error: Some("boom".into()),
            batch: vec![],
            statement_count: None,
            statement_with_output_count: None,
            last_statement_sql: None,
            elapsed_time_ms: None,
        };
        assert!(decode(raw).is_err());
    }

    #[test]
    fn mid_row_cell_count_detected() {
        // 2 columns but only 1 cell -> mid-row termination.
        let batch = make_batch(
            vec![CellType::CellVarint],
            vec![1],
            vec![],
            None,
            vec![],
            true,
        );
        let raw = ProtoQueryResult {
            column_names: vec!["a".into(), "b".into()],
            error: None,
            batch: vec![batch],
            statement_count: None,
            statement_with_output_count: None,
            last_statement_sql: None,
            elapsed_time_ms: None,
        };
        assert!(decode(raw).is_err());
    }
}
