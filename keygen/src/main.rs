//! PDFReader 激活码生成器（图形界面）。
//!
//! 双击 exe 即可用：管理密钥（生成/导入/复制公钥与 Worker 私钥）+ 一键签发激活码。

use pdfreader_keygen::{generate_keypair, hex_to_seed, issue_code, keyinfo_from_seed, load_seed, save_seed};

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([480.0, 600.0])
            .with_min_inner_size([420.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "PDFReader 激活码生成器",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

struct App {
    /// 当前 seed（None 表示尚未生成/导入密钥）。
    seed: Option<[u8; 32]>,
    public_hex: String,
    pkcs8_hex: String,
    order: String,
    last_code: String,
    status: String,
    /// 导入已有 seed 的输入框。
    import_input: String,
    /// 是否在等待「重新生成」二次确认。
    confirm_regen: bool,
}

/// 加载系统中文字体作为兜底，否则中文会显示成方框（egui 默认字体不含 CJK）。
fn install_cjk_font(ctx: &egui::Context) {
    let candidates = [
        r"C:\Windows\Fonts\msyh.ttc",   // 微软雅黑（Windows 默认界面字体）
        r"C:\Windows\Fonts\simhei.ttf", // 黑体
        r"C:\Windows\Fonts\Deng.ttf",   // 等线
        r"C:\Windows\Fonts\simsun.ttc", // 宋体
    ];
    for path in candidates {
        let Ok(bytes) = std::fs::read(path) else { continue };
        let mut fonts = egui::FontDefinitions::default();
        fonts
            .font_data
            .insert("cjk".to_owned(), egui::FontData::from_owned(bytes));
        // 追加到字体族末尾作兜底：拉丁字符仍用 egui 默认字体，缺字时才落到中文字体。
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts
                .families
                .entry(family)
                .or_default()
                .push("cjk".to_owned());
        }
        ctx.set_fonts(fonts);
        return;
    }
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_cjk_font(&cc.egui_ctx);
        let mut app = Self {
            seed: None,
            public_hex: String::new(),
            pkcs8_hex: String::new(),
            order: String::new(),
            last_code: String::new(),
            status: String::new(),
            import_input: String::new(),
            confirm_regen: false,
        };
        app.reload_seed();
        app
    }

    /// 从私钥文件加载 seed；成功则同步填充公钥 / PKCS#8 显示。
    fn reload_seed(&mut self) {
        match load_seed() {
            Ok(seed) => {
                let info = keyinfo_from_seed(&seed);
                self.public_hex = info.public_hex;
                self.pkcs8_hex = info.pkcs8_hex;
                self.seed = Some(seed);
                self.status = "密钥已就绪".to_string();
            }
            Err(e) => {
                self.seed = None;
                self.status = format!("未加载密钥：{e}");
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &eframe::egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("PDFReader 激活码生成器");
            ui.label("生成 / 导入密钥后，一键签发激活码（无时间限制，绑定首次激活的机器）。");
            ui.separator();

            // —— 密钥区 ——
            ui.add_space(4.0);
            ui.strong("① 密钥");
            match self.seed {
                Some(_) => {
                    ui.label(egui::RichText::new("状态：已就绪").color(egui::Color32::from_rgb(0, 140, 80)));
                    key_line(ui, "公钥（粘贴到 license.rs）", &self.public_hex);
                    key_line(ui, "Worker 私钥 PKCS#8（存到 Cloudflare secret，勿外传）", &self.pkcs8_hex);

                    ui.horizontal(|ui| {
                        if ui.button("重新生成密钥").clicked() {
                            self.confirm_regen = true;
                        }
                        if self.confirm_regen {
                            ui.label(egui::RichText::new("会作废所有已发激活码，确定？").color(egui::Color32::RED));
                            if ui.button("确定重新生成").clicked() {
                                match generate_keypair() {
                                    Ok(info) => {
                                        let seed = hex_to_seed(&info.seed_hex);
                                        if let Some(s) = seed {
                                            if let Err(e) = save_seed(&s) {
                                                self.status = e;
                                            } else {
                                                self.public_hex = info.public_hex;
                                                self.pkcs8_hex = info.pkcs8_hex;
                                                self.seed = Some(s);
                                                self.status = "已生成新密钥（旧的已作废）".to_string();
                                            }
                                        }
                                    }
                                    Err(e) => self.status = e,
                                }
                                self.confirm_regen = false;
                            }
                            if ui.button("取消").clicked() {
                                self.confirm_regen = false;
                            }
                        }
                    });
                }
                None => {
                    ui.label(egui::RichText::new("状态：尚未生成密钥").color(egui::Color32::RED));
                    if ui.button("生成新密钥").clicked() {
                        match generate_keypair() {
                            Ok(info) => {
                                let seed = hex_to_seed(&info.seed_hex);
                                if let Some(s) = seed {
                                    if let Err(e) = save_seed(&s) {
                                        self.status = e;
                                    } else {
                                        self.public_hex = info.public_hex;
                                        self.pkcs8_hex = info.pkcs8_hex;
                                        self.seed = Some(s);
                                        self.status = "已生成密钥并写入 private.key（务必备份）".to_string();
                                    }
                                }
                            }
                            Err(e) => self.status = e,
                        }
                    }
                    ui.separator();
                    ui.label("或导入已有密钥（粘贴 seed，即 private.key 里的 64 位 hex）：");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.import_input).desired_width(280.0));
                        if ui.button("导入").clicked() {
                            match hex_to_seed(self.import_input.trim()) {
                                Some(s) => {
                                    if let Err(e) = save_seed(&s) {
                                        self.status = e;
                                    } else {
                                        self.reload_seed();
                                    }
                                }
                                None => self.status = "seed 格式错误（应为 64 位 hex）".to_string(),
                            }
                        }
                    });
                }
            }

            ui.add_space(10.0);
            ui.separator();

            // —— 签发区 ——
            ui.add_space(4.0);
            ui.strong("② 签发激活码");
            ui.horizontal(|ui| {
                ui.label("订单号（可选）");
                ui.add(egui::TextEdit::singleline(&mut self.order).desired_width(180.0));
            });
            let can_issue = self.seed.is_some();
            ui.horizontal(|ui| {
                let gen_clicked = ui
                    .add_enabled(can_issue, egui::Button::new("生成激活码"))
                    .clicked();
                if gen_clicked {
                    if let Some(seed) = self.seed {
                        match issue_code(&seed) {
                            Ok(code) => {
                                self.last_code = code;
                                self.status = "已生成，点击「复制」".to_string();
                            }
                            Err(e) => self.status = e,
                        }
                    }
                }
                if !self.last_code.is_empty() {
                    if ui.button("复制激活码").clicked() {
                        match copy_to_clipboard(&self.last_code) {
                            Ok(()) => self.status = "已复制到剪贴板".to_string(),
                            Err(e) => self.status = format!("复制失败：{e}"),
                        }
                    }
                }
            });

            if !self.last_code.is_empty() {
                ui.add_space(6.0);
                ui.label(egui::RichText::new(&self.last_code).monospace().size(18.0).strong());
            }

            ui.add_space(10.0);
            ui.separator();
            ui.label(egui::RichText::new(&self.status).italics());
        });
    }
}

/// 一行「标题 + 值 + 复制」。
fn key_line(ui: &mut eframe::egui::Ui, title: &str, value: &str) {
    ui.label(title);
    ui.horizontal_wrapped(|ui| {
        ui.add(
            eframe::egui::Label::new(eframe::egui::RichText::new(value).monospace().size(11.0))
                .wrap(),
        );
        if ui.small_button("复制").clicked() {
            let _ = copy_to_clipboard(value);
        }
    });
}

fn copy_to_clipboard(text: &str) -> Result<(), String> {
    arboard::Clipboard::new()
        .and_then(|mut c| c.set_text(text.to_string()))
        .map_err(|e| e.to_string())
}
