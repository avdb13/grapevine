use std::io::Cursor;

use image::imageops::FilterType;
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncWriteExt, BufReader},
};

use crate::{services, Result};

mod data;

pub(crate) use data::Data;

pub(crate) struct FileMeta {
    // This gets written to the database but we no longer read it
    //
    // TODO: Write a database migration to get rid of this and instead store
    // only the filename instead of the entire `Content-Disposition` header.
    #[allow(dead_code)]
    pub(crate) content_disposition: Option<String>,

    pub(crate) content_type: Option<String>,
    pub(crate) file: Vec<u8>,
}

pub(crate) struct Service {
    pub(crate) db: &'static dyn Data,
}

impl Service {
    /// Uploads a file.
    #[tracing::instrument(skip(self, file))]
    pub(crate) async fn create(
        &self,
        mxc: String,
        content_disposition: Option<&str>,
        content_type: Option<&str>,
        file: &[u8],
    ) -> Result<()> {
        // Width, Height = 0 if it's not a thumbnail
        let key = self.db.create_file_metadata(
            mxc,
            0,
            0,
            content_disposition,
            content_type,
        )?;

        let path = services().globals.get_media_file(&key);
        let mut f = File::create(path).await?;
        f.write_all(file).await?;
        Ok(())
    }

    /// Uploads or replaces a file thumbnail.
    #[allow(clippy::too_many_arguments)]
    #[tracing::instrument(skip(self, file))]
    pub(crate) async fn upload_thumbnail(
        &self,
        mxc: String,
        content_disposition: Option<&str>,
        content_type: Option<&str>,
        width: u32,
        height: u32,
        file: &[u8],
    ) -> Result<()> {
        let key = self.db.create_file_metadata(
            mxc,
            width,
            height,
            content_disposition,
            content_type,
        )?;

        let path = services().globals.get_media_file(&key);
        let mut f = File::create(path).await?;
        f.write_all(file).await?;

        Ok(())
    }

    /// Downloads a file.
    #[tracing::instrument(skip(self))]
    pub(crate) async fn get(&self, mxc: String) -> Result<Option<FileMeta>> {
        if let Ok((content_disposition, content_type, key)) =
            self.db.search_file_metadata(mxc, 0, 0)
        {
            let path = services().globals.get_media_file(&key);
            let mut file = Vec::new();
            BufReader::new(File::open(path).await?)
                .read_to_end(&mut file)
                .await?;

            Ok(Some(FileMeta {
                content_disposition,
                content_type,
                file,
            }))
        } else {
            Ok(None)
        }
    }

    /// Returns width, height of the thumbnail and whether it should be cropped.
    /// Returns None when the server should send the original file.
    fn thumbnail_properties(
        width: u32,
        height: u32,
    ) -> Option<(u32, u32, bool)> {
        match (width, height) {
            (0..=32, 0..=32) => Some((32, 32, true)),
            (0..=96, 0..=96) => Some((96, 96, true)),
            (0..=320, 0..=240) => Some((320, 240, false)),
            (0..=640, 0..=480) => Some((640, 480, false)),
            (0..=800, 0..=600) => Some((800, 600, false)),
            _ => None,
        }
    }

    /// Downloads a file's thumbnail.
    ///
    /// Here's an example on how it works:
    ///
    /// - Client requests an image with width=567, height=567
    /// - Server rounds that up to (800, 600), so it doesn't have to save too
    ///   many thumbnails
    /// - Server rounds that up again to (958, 600) to fix the aspect ratio
    ///   (only for width,height>96)
    /// - Server creates the thumbnail and sends it to the user
    ///
    /// For width,height <= 96 the server uses another thumbnailing algorithm
    /// which crops the image afterwards.
    #[allow(clippy::too_many_lines)]
    #[tracing::instrument(skip(self))]
    pub(crate) async fn get_thumbnail(
        &self,
        mxc: String,
        width: u32,
        height: u32,
    ) -> Result<Option<FileMeta>> {
        // 0, 0 because that's the original file
        let (width, height, crop) =
            Self::thumbnail_properties(width, height).unwrap_or((0, 0, false));

        if let Ok((content_disposition, content_type, key)) =
            self.db.search_file_metadata(mxc.clone(), width, height)
        {
            // Using saved thumbnail
            let path = services().globals.get_media_file(&key);
            let mut file = Vec::new();
            File::open(path).await?.read_to_end(&mut file).await?;

            Ok(Some(FileMeta {
                content_disposition,
                content_type,
                file: file.clone(),
            }))
        } else if let Ok((content_disposition, content_type, key)) =
            self.db.search_file_metadata(mxc.clone(), 0, 0)
        {
            // Generate a thumbnail
            let path = services().globals.get_media_file(&key);
            let mut file = Vec::new();
            File::open(path).await?.read_to_end(&mut file).await?;

            if let Ok(image) = image::load_from_memory(&file) {
                let original_width = image.width();
                let original_height = image.height();
                if width > original_width || height > original_height {
                    return Ok(Some(FileMeta {
                        content_disposition,
                        content_type,
                        file: file.clone(),
                    }));
                }

                let thumbnail = if crop {
                    image.resize_to_fill(width, height, FilterType::CatmullRom)
                } else {
                    let (exact_width, exact_height) = {
                        // Copied from image::dynimage::resize_dimensions
                        let use_width = (u64::from(width)
                            * u64::from(original_height))
                            <= (u64::from(original_width) * u64::from(height));
                        let intermediate = if use_width {
                            u64::from(original_height) * u64::from(width)
                                / u64::from(original_width)
                        } else {
                            u64::from(original_width) * u64::from(height)
                                / u64::from(original_height)
                        };
                        if use_width {
                            if intermediate <= u64::from(::std::u32::MAX) {
                                (
                                    width,
                                    intermediate.try_into().unwrap_or(u32::MAX),
                                )
                            } else {
                                (
                                    (u64::from(width)
                                        * u64::from(::std::u32::MAX)
                                        / intermediate)
                                        .try_into()
                                        .unwrap_or(u32::MAX),
                                    ::std::u32::MAX,
                                )
                            }
                        } else if intermediate <= u64::from(::std::u32::MAX) {
                            (
                                intermediate.try_into().unwrap_or(u32::MAX),
                                height,
                            )
                        } else {
                            (
                                ::std::u32::MAX,
                                (u64::from(height)
                                    * u64::from(::std::u32::MAX)
                                    / intermediate)
                                    .try_into()
                                    .unwrap_or(u32::MAX),
                            )
                        }
                    };

                    image.thumbnail_exact(exact_width, exact_height)
                };

                let mut thumbnail_bytes = Vec::new();
                thumbnail.write_to(
                    &mut Cursor::new(&mut thumbnail_bytes),
                    image::ImageFormat::Png,
                )?;

                // Save thumbnail in database so we don't have to generate it
                // again next time
                let thumbnail_key = self.db.create_file_metadata(
                    mxc,
                    width,
                    height,
                    content_disposition.as_deref(),
                    content_type.as_deref(),
                )?;

                let path = services().globals.get_media_file(&thumbnail_key);
                let mut f = File::create(path).await?;
                f.write_all(&thumbnail_bytes).await?;

                Ok(Some(FileMeta {
                    content_disposition,
                    content_type,
                    file: thumbnail_bytes.clone(),
                }))
            } else {
                // Couldn't parse file to generate thumbnail, send original
                Ok(Some(FileMeta {
                    content_disposition,
                    content_type,
                    file: file.clone(),
                }))
            }
        } else {
            Ok(None)
        }
    }
}
