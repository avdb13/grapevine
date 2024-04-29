pub mod read_receipt;
pub mod typing;

pub trait Data: read_receipt::Data + 'static {}

pub struct Service {
    pub read_receipt: read_receipt::Service,
    pub typing: typing::Service,
}
