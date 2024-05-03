use egui::Widget;
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use std::{collections::HashMap, fmt::Debug};

/// We derive Deserialize/Serialize so we can persist app state on shutdown.
// #[derive(serde::Deserialize, serde::Serialize)]
// #[serde(default)] // if we add new fields, give them default values when deserializing old state
pub struct BlogApp {
    pages_menu_open: bool,
    pages: Vec<(String, String)>,
    cache: CommonMarkCache
}

impl Default for BlogApp {
    fn default() -> Self {
        // walkdir::WalkDir::new("../assets/articles").into_iter().filter_map(|e| Result::ok(e).map(|e| e.path())).filter(|e| {
        //     e.extension().is_some_and(|ext| ext == "md".into())
        // }).for_each(|p| {
        //         p.components().find() 
        // });
        Self {
            pages_menu_open: false,
            pages: vec![("test".to_string(), include_str!("../assets/articles/hello.md").to_string())],
            cache: CommonMarkCache::default()
        }
    }

}

impl BlogApp {
    /// Called once before the first frame.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // This is also where you can customize the look and feel of egui using
        // `cc.egui_ctx.set_visuals` and `cc.egui_ctx.set_fonts`.

        // Load previous app state (if any).
        // Note that you must enable the `persistence` feature for this to work.
        // if let Some(storage) = cc.storage {
        //     return eframe::get_value(storage, eframe::APP_KEY).unwrap_or_default();
        // }

        Default::default()
    }

    fn pages_panel(&mut self, ctx: &egui::Context, fame: &mut eframe::Frame) {
        egui::SidePanel::left("backend_panel")
            .resizable(false)
            .show_animated(ctx, self.pages_menu_open, |ui| {
                ui.vertical_centered(|ui| {
                    ui.heading("Pages");
                });

                ui.separator();
                for (name, _) in &self.pages {
                    if ui.label(format!("{name}")).clicked() {
                        log::info!("moving to xyz");
                    };
            }});
    }

    fn bar_contents(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::widgets::global_dark_light_mode_switch(ui);

        ui.separator();

        ui.toggle_value(&mut self.pages_menu_open, "Pages");

        ui.separator();
    }
}

impl eframe::App for BlogApp {
    /// Called by the frame work to save state before shutdown.
    // fn save(&mut self, storage: &mut dyn eframe::Storage) {
    //     eframe::set_value(storage, eframe::APP_KEY, self);
    // }

    /// Called each time the UI needs repainting, which may be many times per second.
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Put your widgets into a `SidePanel`, `TopBottomPanel`, `CentralPanel`, `Window` or `Area`.
        // For inspiration and more examples, go to https://emilk.github.io/egui
        egui::SidePanel::left("pages_panel").min_width(0.0).default_width(0.0).show_animated(ctx, self.pages_menu_open, |ui| {
            
            // The top panel is often a good place for a menu bar:
            egui::TopBottomPanel::top("blog_app_top_bar").show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.visuals_mut().button_frame = false;
                    self.bar_contents(ui, frame);
                });
            });
            egui::menu::bar(ui, |ui| {
                // NOTE: no File->Quit on web pages!
                let is_web = cfg!(target_arch = "wasm32");
                if !is_web {
                    ui.menu_button("File", |ui| {
                        if ui.button("Quit").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                    ui.add_space(16.0);
                }

                egui::widgets::global_dark_light_mode_buttons(ui);
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.menu_button("Pages", |ui| ui.label("test"));
            // The central panel the region left after adding TopPanel's and SidePanel's
            ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                ui.heading("Home");
                
                let mut cache = CommonMarkCache::default();

                CommonMarkViewer::new("viewer").show(ui, &mut cache, include_str!("../assets/articles/hello.md") );

                ui.separator();

                ui.add(egui::github_link_file!(
                    "https://github.com/louisponet/blog/tree/main/",
                    "Source code."
                ));

                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    powered_by_egui_and_eframe(ui);
                });
            });
        });
    }
}

fn powered_by_egui_and_eframe(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.label("Powered by ");
        ui.hyperlink_to("egui", "https://github.com/emilk/egui");
        ui.label(" and ");
        ui.hyperlink_to(
            "eframe",
            "https://github.com/emilk/egui/tree/master/crates/eframe",
        );
        ui.label(".");
    });
}
