use crate::types::SqlType;

#[derive(Debug)]
pub enum LogicalPlan {}

impl LogicalPlan {
    pub fn schema(&self) -> Option<&PlanSchema> {
        None
    }
}

pub struct PlanSchema {
    pub columns: Vec<PlanColumn>,
}

pub struct PlanColumn {
    pub name: String,
    pub sql_type: SqlType,
    pub nullable: bool,
}
