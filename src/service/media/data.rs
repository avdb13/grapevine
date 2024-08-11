use super::{FileMeta, MediaFileKey};
use crate::Result;

pub(crate) trait Data: Send + Sync {
    fn create_file_metadata(
        &self,
        mxc: String,
        width: u32,
        height: u32,
        meta: &FileMeta,
    ) -> Result<MediaFileKey>;

    fn search_file_metadata(
        &self,
        mxc: String,
        width: u32,
        height: u32,
    ) -> Result<(FileMeta, MediaFileKey)>;
}
