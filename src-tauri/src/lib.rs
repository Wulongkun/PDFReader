mod app;
mod config;
mod library;
mod license;
mod orientation;
mod pdf;
mod translate;
mod ui;

use app::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![
            ui::commands::open_pdf,
            ui::commands::open_pdf_path,
            ui::commands::read_pdf,
            ui::commands::read_pdf_range,
            ui::commands::pdf_file_size,
            ui::commands::get_library,
            ui::commands::add_books_dialog,
            ui::commands::add_folder_dialog,
            ui::commands::remove_book,
            ui::commands::remove_recent,
            ui::commands::list_folder,
            ui::commands::toggle_favorite,
            ui::commands::record_recent,
            ui::commands::get_last_page,
            ui::commands::set_last_page,
            ui::commands::get_orientation,
            ui::commands::set_orientation,
            ui::commands::list_pdfs_recursive,
            ui::commands::save_thumb,
            ui::commands::load_thumb,
            ui::commands::extract_pages,
            ui::commands::extract_text,
            ui::commands::translate,
            ui::commands::translate_stream,
            ui::commands::get_config,
            ui::commands::set_config,
            ui::commands::activate_license,
            ui::commands::get_license_status,
            ui::commands::ocr_image,
            ui::commands::ocr_image_local,
            ui::commands::extract_page,
            ui::commands::export_text,
            ui::commands::export_docx,
            ui::commands::export_word_native,
            ui::commands::export_word_translated_native,
            ui::commands::log_diag,
            ui::commands::save_ocr_raw,
            ui::commands::open_external,
            ui::commands::translate_paragraphs,
        ])
        .run(tauri::generate_context!())
        .expect("启动 Tauri 应用失败");
}
