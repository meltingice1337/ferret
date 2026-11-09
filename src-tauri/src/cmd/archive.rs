//! Archives, pictures and videos, spreadsheets and slides.

use ferret_core::archive::{self, ArchiveInfo};
use ferret_core::media::{self, AudioInfo, ImageInfo, ImageJob, ImageOpts, ImagePreview, VideoInfo, VideoJob, VideoSample};
use ferret_core::office::{self, Deck, SheetPreview};

type Res<T> = Result<T, String>;

#[tauri::command]
pub async fn archive_materialize(path: String, password: Option<String>) -> Res<String> {
    archive::archive_materialize(path, password).await
}

#[tauri::command]
pub async fn archive_extract(archive: String, dest: String, entries: Option<Vec<String>>, password: Option<String>) -> Res<u64> {
    archive::archive_extract(archive, dest, entries, password).await
}

#[tauri::command]
pub async fn archive_extract_smart(archive: String, password: Option<String>) -> Res<u64> {
    archive::archive_extract_smart(archive, password).await
}

#[tauri::command]
pub fn compress_zip(sources: Vec<String>, dest: String) -> Res<u64> {
    archive::compress_zip(sources, dest)
}

#[tauri::command]
pub async fn archive_info(path: String) -> Res<ArchiveInfo> {
    archive::archive_info(path).await
}

#[tauri::command]
pub async fn archive_unlock(path: String, password: String) -> Res<()> {
    archive::archive_unlock(path, password).await
}

#[tauri::command]
pub async fn video_info(path: String) -> Res<VideoInfo> {
    media::video_info(path).await
}

#[tauri::command]
pub async fn audio_info(path: String) -> Res<AudioInfo> {
    media::audio_info(path).await
}

#[tauri::command]
pub fn compress_videos(items: Vec<VideoJob>) -> Res<u64> {
    media::compress_videos(items)
}

#[tauri::command]
pub async fn video_sample(job: VideoJob, at_ms: u64, length_ms: u64) -> Res<Option<VideoSample>> {
    media::video_sample(job, at_ms, length_ms).await
}

#[tauri::command]
pub async fn image_info(path: String) -> Res<ImageInfo> {
    media::image_info(path).await
}

#[tauri::command]
pub async fn image_preview(path: String, opts: ImageOpts) -> Res<ImagePreview> {
    media::image_preview(path, opts).await
}

#[tauri::command]
pub fn compress_images(items: Vec<ImageJob>, opts: ImageOpts) -> Res<u64> {
    media::compress_images(items, opts)
}

#[tauri::command]
pub fn media_release() {
    media::media_release()
}

#[tauri::command]
pub async fn sheet_preview(path: String, sheet: Option<usize>) -> Res<SheetPreview> {
    office::sheet_preview(path, sheet).await
}

#[tauri::command]
pub async fn slides_preview(path: String) -> Res<Deck> {
    office::slides_preview(path).await
}
