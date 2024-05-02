pub(crate) mod read_receipt;
pub(crate) mod typing;

pub(crate) trait Data: read_receipt::Data + 'static {}

pub(crate) struct Service {
    pub(crate) read_receipt: read_receipt::Service,
    pub(crate) typing: typing::Service,
}
