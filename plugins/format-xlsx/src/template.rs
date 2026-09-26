use dre_protocol::plugin::{Result, ResultSets, WriteRequest};
pub fn fill(_req: &WriteRequest, _sets: &mut ResultSets<'_>) -> Result<Vec<String>> {
    Err("xlsx templates are not supported yet".into())
}
