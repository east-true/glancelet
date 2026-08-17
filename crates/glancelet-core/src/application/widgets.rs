use std::collections::HashSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{GlanceletError, Result};

use super::WorkStore;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WidgetType {
    Today,
    Inbox,
    Upcoming,
    Attention,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WidgetSize {
    Compact,
    Wide,
    Tall,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WidgetInstance {
    pub widget_type: WidgetType,
    pub position: i64,
    pub size: WidgetSize,
    #[serde(default = "empty_settings")]
    pub settings: Value,
}

fn empty_settings() -> Value {
    json!({})
}

pub fn default_widget_layout() -> Vec<WidgetInstance> {
    vec![
        WidgetInstance {
            widget_type: WidgetType::Today,
            position: 0,
            size: WidgetSize::Wide,
            settings: empty_settings(),
        },
        WidgetInstance {
            widget_type: WidgetType::Inbox,
            position: 1,
            size: WidgetSize::Compact,
            settings: empty_settings(),
        },
        WidgetInstance {
            widget_type: WidgetType::Attention,
            position: 2,
            size: WidgetSize::Compact,
            settings: empty_settings(),
        },
    ]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopPreferences {
    pub always_on_top: bool,
    pub global_shortcut_enabled: bool,
    pub privacy_mode: bool,
}

impl Default for DesktopPreferences {
    fn default() -> Self {
        Self {
            always_on_top: false,
            global_shortcut_enabled: true,
            privacy_mode: false,
        }
    }
}

pub struct WidgetLayoutService {
    store: Arc<dyn WorkStore>,
}

impl WidgetLayoutService {
    pub fn new(store: Arc<dyn WorkStore>) -> Self {
        Self { store }
    }

    pub fn layout(&self) -> Result<Vec<WidgetInstance>> {
        self.store.widget_layout()
    }

    pub fn save(&self, widgets: &[WidgetInstance]) -> Result<()> {
        validate_layout(widgets)?;
        let normalized = widgets
            .iter()
            .enumerate()
            .map(|(position, widget)| WidgetInstance {
                widget_type: widget.widget_type,
                position: position as i64,
                size: widget.size,
                settings: widget.settings.clone(),
            })
            .collect::<Vec<_>>();
        self.store.save_widget_layout(&normalized)
    }

    pub fn preferences(&self) -> Result<DesktopPreferences> {
        self.store.desktop_preferences()
    }

    pub fn set_always_on_top(&self, always_on_top: bool) -> Result<DesktopPreferences> {
        let mut preferences = self.preferences()?;
        preferences.always_on_top = always_on_top;
        self.store.save_desktop_preferences(&preferences)?;
        Ok(preferences)
    }

    pub fn set_global_shortcut_enabled(&self, enabled: bool) -> Result<DesktopPreferences> {
        let mut preferences = self.preferences()?;
        preferences.global_shortcut_enabled = enabled;
        self.store.save_desktop_preferences(&preferences)?;
        Ok(preferences)
    }

    pub fn set_privacy_mode(&self, enabled: bool) -> Result<DesktopPreferences> {
        let mut preferences = self.preferences()?;
        preferences.privacy_mode = enabled;
        self.store.save_desktop_preferences(&preferences)?;
        Ok(preferences)
    }
}

fn validate_layout(widgets: &[WidgetInstance]) -> Result<()> {
    if widgets.is_empty() {
        return Err(GlanceletError::InvalidOperation(
            "the Desktop Surface must contain at least one widget".into(),
        ));
    }
    let mut types = HashSet::new();
    for widget in widgets {
        if !types.insert(widget.widget_type) {
            return Err(GlanceletError::InvalidOperation(
                "a built-in widget can only be added once".into(),
            ));
        }
        if !widget.settings.is_object() {
            return Err(GlanceletError::InvalidOperation(
                "widget settings must be an object".into(),
            ));
        }
    }
    Ok(())
}
