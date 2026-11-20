//! Builds and repaints the menu bar menu.
//!
//! The menu is a pure view of `MenuState`: every row is created once, and later
//! snapshots only rewrite text and check marks. Item ids are stable, so the event
//! loop can map a click back to an `Action`.

use std::collections::BTreeMap;

use muda::{
    accelerator::Accelerator, CheckMenuItem, ContextMenu, Menu, MenuId, MenuItem,
    PredefinedMenuItem, Submenu,
};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::actions::{Action, MenuState};

/// The single-color mark AppKit uses as a template image.
const ICON_PNG: &[u8] = include_bytes!("../assets/trayTemplate.png");

const STATUS_ID: &str = "status";
const MODEL_ID: &str = "model";
const USAGE_ID: &str = "usage";
const ERROR_ID: &str = "error";
const TOGGLE_ID: &str = "toggle";
const MODELS_ID: &str = "models";
const RELOAD_ID: &str = "reload";
const EXPORT_ID: &str = "export";
const OPEN_CONFIG_ID: &str = "open-config";
const OPEN_LOGS_ID: &str = "open-logs";
const QUIT_ID: &str = "quit";

/// A model row's id carries its hash, the value `config.toml` stores.
const MODEL_ITEM_PREFIX: &str = "model:";

pub struct Tray {
    _icon: TrayIcon,
    status: MenuItem,
    model: MenuItem,
    usage: MenuItem,
    error: MenuItem,
    toggle: MenuItem,
    models: Submenu,
    /// Check items keyed by `model_hash`, so a snapshot moves the tick.
    model_items: BTreeMap<String, CheckMenuItem>,
}

impl Tray {
    /// Creates the icon and its menu. Must run on the main (AppKit) thread.
    pub fn install() -> Result<Self, Box<dyn std::error::Error>> {
        let status = MenuItem::with_id(STATUS_ID, "状态: 启动中…", false, None::<Accelerator>);
        let model = MenuItem::with_id(MODEL_ID, "默认模型: —", false, None::<Accelerator>);
        let usage = MenuItem::with_id(
            USAGE_ID,
            "今日 Token: — | 本月: —",
            false,
            None::<Accelerator>,
        );
        let error = MenuItem::with_id(ERROR_ID, "最近错误: —", true, None::<Accelerator>);
        let toggle = MenuItem::with_id(TOGGLE_ID, "暂停接管", true, None::<Accelerator>);
        let models = Submenu::with_id(MODELS_ID, "切换默认模型", true);
        let reload = MenuItem::with_id(RELOAD_ID, "重载配置", true, None::<Accelerator>);
        let export = MenuItem::with_id(EXPORT_ID, "导出用量 CSV…", true, None::<Accelerator>);
        let open_config =
            MenuItem::with_id(OPEN_CONFIG_ID, "打开配置文件", true, None::<Accelerator>);
        let open_logs = MenuItem::with_id(OPEN_LOGS_ID, "打开日志目录", true, None::<Accelerator>);
        let quit = MenuItem::with_id(QUIT_ID, "退出", true, None::<Accelerator>);

        let menu = Menu::new();
        menu.append_items(&[
            &status,
            &model,
            &usage,
            &error,
            &PredefinedMenuItem::separator(),
            &toggle,
            &models,
            &reload,
            &export,
            &open_config,
            &open_logs,
            &PredefinedMenuItem::separator(),
            &quit,
        ])?;

        let icon = TrayIconBuilder::new()
            .with_icon(load_icon()?)
            .with_icon_as_template(true)
            .with_tooltip("Cursor BYOK Lite")
            .with_menu(Box::new(menu) as Box<dyn ContextMenu>)
            .with_menu_on_left_click(true)
            .build()?;

        Ok(Self {
            _icon: icon,
            status,
            model,
            usage,
            error,
            toggle,
            models,
            model_items: BTreeMap::new(),
        })
    }

    /// Rewrites every row from a snapshot.
    pub fn apply(&mut self, state: &MenuState) {
        self.status.set_text(&state.status);
        self.model.set_text(&state.model);
        self.usage.set_text(&state.usage);
        match &state.error {
            Some(message) => {
                self.error.set_text(message);
                // The row only exists while there is something to report.
                self.error.set_enabled(true);
            }
            // An empty title is still a clickable row, so it is disabled and
            // says nothing rather than claiming "no error" it cannot know.
            None => {
                self.error.set_text("");
                self.error.set_enabled(false);
            }
        }
        self.toggle.set_text(if state.taking_over {
            "暂停接管"
        } else {
            "开启接管"
        });
        for (hash, item) in &self.model_items {
            item.set_checked(*hash == state.default_model);
        }
    }

    /// Shows a failure where the error row lives, leaving the rest untouched.
    pub fn fail(&mut self, message: &str) {
        self.error.set_text(format!("最近错误: {message}"));
        self.error.set_enabled(true);
    }

    /// Replaces the model submenu when the catalog changed.
    ///
    /// Returns whether anything was rebuilt, so the caller can repaint the ticks.
    pub fn sync_models(&mut self, catalog: &[(String, String)], default_model: &str) -> bool {
        let same = self.model_items.len() == catalog.len()
            && catalog
                .iter()
                .all(|(hash, _)| self.model_items.contains_key(hash));
        if same {
            return false;
        }
        for _ in 0..self.model_items.len() {
            self.models.remove_at(0);
        }
        self.model_items.clear();
        for (hash, display_name) in catalog {
            let item = CheckMenuItem::with_id(
                format!("{MODEL_ITEM_PREFIX}{hash}"),
                display_name,
                true,
                hash == default_model,
                None::<Accelerator>,
            );
            if self.models.append(&item).is_ok() {
                self.model_items.insert(hash.clone(), item);
            }
        }
        true
    }

    /// Maps a clicked item back to an action.
    pub fn action_for(&self, id: &MenuId) -> Action {
        let id = id.as_ref();
        match id {
            TOGGLE_ID => Action::ToggleTakeover,
            RELOAD_ID => Action::ReloadConfig,
            EXPORT_ID => Action::ExportCsv,
            OPEN_CONFIG_ID => Action::OpenConfig,
            OPEN_LOGS_ID | ERROR_ID => Action::OpenLogs,
            QUIT_ID => Action::Quit,
            other => match other.strip_prefix(MODEL_ITEM_PREFIX) {
                Some(model_hash) => Action::SelectModel(model_hash.to_owned()),
                None => Action::Ignore,
            },
        }
    }
}

/// Decodes the bundled template icon; AppKit renders it as a mask.
fn load_icon() -> Result<Icon, Box<dyn std::error::Error>> {
    let decoder = png::Decoder::new(std::io::Cursor::new(ICON_PNG));
    let mut reader = decoder.read_info()?;
    let size = reader
        .output_buffer_size()
        .ok_or("the tray icon dimensions are too large")?;
    let mut buffer = vec![0; size];
    let info = reader.next_frame(&mut buffer)?;
    buffer.truncate(info.buffer_size());
    if info.color_type != png::ColorType::Rgba {
        return Err("the tray icon must be RGBA".into());
    }
    Ok(Icon::from_rgba(buffer, info.width, info.height)?)
}
