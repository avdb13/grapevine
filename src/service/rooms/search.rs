mod data;

pub(crate) use data::Data;
pub(crate) type Service = &'static dyn Data;
