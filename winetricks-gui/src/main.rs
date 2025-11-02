//! Winetricks GUI
//!
//! Modern, cross-platform GUI built with Iced (default) or libcosmic (COSMIC desktop)

#[cfg(feature = "cosmic")]
mod cosmic_app;

#[cfg(feature = "iced")]
use iced::widget::{
    button, checkbox, column, container, pick_list, row, scrollable, text, text_input,
};
#[cfg(feature = "iced")]
use iced::{Alignment, Color, Element, Length, Pixels, Sandbox, Settings, Theme};
use winetricks_lib::{Config, VerbCategory, VerbRegistry};

#[cfg(feature = "iced")]
fn main() -> iced::Result {
    WinetricksApp::run(Settings {
        window: iced::window::Settings {
            size: iced::Size::new(1000.0, 750.0),
            resizable: true,
            min_size: Some(iced::Size::new(800.0, 600.0)),
            ..Default::default()
        },
        default_font: iced::Font::default(),
        default_text_size: Pixels(14.0),
        antialiasing: true,
        ..Default::default()
    })
}

#[cfg(feature = "cosmic")]
#[cfg(not(feature = "iced"))]
fn main() -> cosmic::Result {
    cosmic_app::cosmic_impl::run_cosmic()
}

#[cfg(all(feature = "cosmic", feature = "iced"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Check if we should use Cosmic (if COSMIC desktop is detected)
    if std::env::var("COSMIC_SESSION").is_ok() || std::env::var("XDG_CURRENT_DESKTOP").map(|v| v.contains("COSMIC")).unwrap_or(false) {
        cosmic_app::cosmic_impl::run_cosmic()?;
        Ok(())
    } else {
        // Default to Iced
        WinetricksApp::run(Settings {
            window: iced::window::Settings {
                size: iced::Size::new(1000.0, 750.0),
                resizable: true,
                min_size: Some(iced::Size::new(800.0, 600.0)),
                ..Default::default()
            },
            default_font: iced::Font::default(),
            default_text_size: Pixels(14.0),
            antialiasing: true,
            ..Default::default()
        })?;
        Ok(())
    }
}

#[cfg(not(any(feature = "iced", feature = "cosmic")))]
fn main() {
    eprintln!("Error: No GUI backend enabled. Please build with --features iced or --features cosmic");
    std::process::exit(1);
}

struct WinetricksApp {
    config: Config,
    registry: VerbRegistry,
    current_view: View,
    search_query: String,
    selected_category: Option<VerbCategory>,
    installed_verbs: Vec<String>,
    // Preference state
    wineprefix_input: String,
    country_input: String,
    winearch_selection: Option<WineArch>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Browse,
    Installed,
    Preferences,
    WineTools,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WineArch {
    Auto,
    Win32,
    Win64,
}

impl WineArch {
    fn all() -> [WineArch; 3] {
        [WineArch::Auto, WineArch::Win32, WineArch::Win64]
    }
}

impl std::fmt::Display for WineArch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WineArch::Auto => write!(f, "Auto"),
            WineArch::Win32 => write!(f, "32-bit (win32)"),
            WineArch::Win64 => write!(f, "64-bit (win64)"),
        }
    }
}

#[derive(Debug, Clone)]
enum Message {
    ViewChanged(View),
    SearchChanged(String),
    CategorySelected(VerbCategory),
    InstallVerb(String),
    UninstallVerb(String),
    // Wine Tools messages
    RunWineTool(String),
    // Preference settings messages
    WineprefixChanged(String),
    BrowseWineprefix,
    CountryChanged(String),
    WinearchChanged(WineArch),
    ForceToggled(bool),
    UnattendedToggled(bool),
    TorifyToggled(bool),
    IsolateToggled(bool),
    NoCleanToggled(bool),
    VerbosityChanged(u8),
}

// Modern dark theme colors
mod colors {
    use iced::Color;

    pub const BACKGROUND: Color = Color::from_rgb(0.08, 0.08, 0.1);
    pub const SURFACE: Color = Color::from_rgb(0.12, 0.12, 0.15);
    pub const SURFACE_HOVER: Color = Color::from_rgb(0.16, 0.16, 0.2);
    pub const PRIMARY: Color = Color::from_rgb(0.4, 0.7, 1.0);
    pub const PRIMARY_HOVER: Color = Color::from_rgb(0.5, 0.75, 1.0);
    pub const TEXT_PRIMARY: Color = Color::from_rgb(1.0, 1.0, 1.0);
    pub const TEXT_SECONDARY: Color = Color::from_rgb(0.7, 0.7, 0.75);
    pub const TEXT_DIM: Color = Color::from_rgb(0.5, 0.5, 0.55);
}

impl Sandbox for WinetricksApp {
    type Message = Message;

    fn new() -> Self {
        // Initialize configuration
        let config = Config::new().unwrap_or_else(|_| Config::default());
        let metadata_dir = config.metadata_dir();

        // Load verb registry
        let registry = if metadata_dir.exists() {
            VerbRegistry::load_from_dir(metadata_dir).unwrap_or_else(|_| VerbRegistry::new())
        } else {
            VerbRegistry::new()
        };

        // Load installed verbs
        let installed_verbs = load_installed_verbs(&config);

        // Initialize preference state
        let wineprefix_input = config.wineprefix().to_string_lossy().to_string();
        let winearch_selection = match config.winearch.as_deref() {
            Some("win32") => Some(WineArch::Win32),
            Some("win64") => Some(WineArch::Win64),
            _ => Some(WineArch::Auto),
        };

        Self {
            config,
            registry,
            current_view: View::Browse,
            search_query: String::new(),
            selected_category: None,
            installed_verbs,
            wineprefix_input,
            country_input: String::new(),
            winearch_selection,
        }
    }

    fn title(&self) -> String {
        "Winetricks".to_string()
    }

    fn update(&mut self, message: Message) {
        match message {
            Message::ViewChanged(view) => {
                self.current_view = view;
            }
            Message::SearchChanged(query) => {
                self.search_query = query;
            }
            Message::CategorySelected(category) => {
                self.selected_category = Some(category);
            }
            Message::InstallVerb(verb_name) => {
                eprintln!("Install verb: {}", verb_name);
                // TODO: Implement async installation
            }
            Message::UninstallVerb(verb_name) => {
                eprintln!("Uninstall verb: {}", verb_name);
                // TODO: Implement async uninstallation
            }
            Message::RunWineTool(tool) => {
                eprintln!("Running Wine tool: {}", tool);
                run_wine_tool(&self.config, &tool);
            }
            // Preference settings updates
            Message::WineprefixChanged(value) => {
                self.wineprefix_input = value.clone();
                // Update config with new wineprefix
                let new_path = if let Ok(path) = std::path::PathBuf::from(&value).canonicalize() {
                    path
                } else {
                    std::path::PathBuf::from(&value)
                };
                self.config.wineprefix = Some(new_path);
                // Reload installed verbs when prefix changes
                self.installed_verbs = load_installed_verbs(&self.config);
            }
            Message::BrowseWineprefix => {
                // Open native folder picker dialog
                use std::process;

                let current_prefix = std::path::PathBuf::from(&self.wineprefix_input);
                let start_dir = if current_prefix.exists() {
                    current_prefix.clone()
                } else {
                    current_prefix
                        .parent()
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|| {
                            dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"))
                        })
                };

                // Try native file pickers: zenity (GNOME), kdialog (KDE), yad (generic)
                let start_path = start_dir.to_string_lossy().to_string();

                // Try zenity first (GNOME)
                if let Ok(output) = process::Command::new("zenity")
                    .arg("--file-selection")
                    .arg("--directory")
                    .arg("--title=Select Wineprefix Directory")
                    .arg(format!("--filename={}", start_path))
                    .output()
                {
                    if output.status.success() {
                        let selected = String::from_utf8_lossy(&output.stdout).trim().to_string();
                        if !selected.is_empty() {
                            self.wineprefix_input = selected.clone();
                            // Update config
                            let new_path = if let Ok(path) =
                                std::path::PathBuf::from(&selected).canonicalize()
                            {
                                path
                            } else {
                                std::path::PathBuf::from(&selected)
                            };
                            self.config.wineprefix = Some(new_path);
                            // Reload installed verbs for the new prefix
                            self.installed_verbs = load_installed_verbs(&self.config);
                            return;
                        }
                    }
                }

                // Try kdialog (KDE)
                if let Ok(output) = process::Command::new("kdialog")
                    .arg("--getexistingdirectory")
                    .arg(&start_path)
                    .arg("--title")
                    .arg("Select Wineprefix Directory")
                    .output()
                {
                    if output.status.success() {
                        let selected = String::from_utf8_lossy(&output.stdout).trim().to_string();
                        if !selected.is_empty() && !selected.starts_with("Error:") {
                            self.wineprefix_input = selected.clone();
                            // Update config
                            let new_path = if let Ok(path) =
                                std::path::PathBuf::from(&selected).canonicalize()
                            {
                                path
                            } else {
                                std::path::PathBuf::from(&selected)
                            };
                            self.config.wineprefix = Some(new_path);
                            // Reload installed verbs for the new prefix
                            self.installed_verbs = load_installed_verbs(&self.config);
                            return;
                        }
                    }
                }

                // Try yad (generic, works on many systems)
                if let Ok(output) = process::Command::new("yad")
                    .arg("--file")
                    .arg("--directory")
                    .arg("--title=Select Wineprefix Directory")
                    .arg(format!("--filename={}", start_path))
                    .output()
                {
                    if output.status.success() {
                        let selected = String::from_utf8_lossy(&output.stdout).trim().to_string();
                        if !selected.is_empty() {
                            self.wineprefix_input = selected.clone();
                            // Update config
                            let new_path = if let Ok(path) =
                                std::path::PathBuf::from(&selected).canonicalize()
                            {
                                path
                            } else {
                                std::path::PathBuf::from(&selected)
                            };
                            self.config.wineprefix = Some(new_path);
                            // Reload installed verbs for the new prefix
                            self.installed_verbs = load_installed_verbs(&self.config);
                            return;
                        }
                    }
                }

                // Fallback: open file manager and show error
                eprintln!(
                    "No file picker found (zenity/kdialog/yad). Opening file manager instead."
                );
                let file_managers = ["xdg-open", "open", "cygstart"];
                for fm in &file_managers {
                    if process::Command::new(fm).arg(&start_dir).spawn().is_ok() {
                        break;
                    }
                }
            }
            Message::CountryChanged(value) => {
                self.country_input = value;
            }
            Message::WinearchChanged(arch) => {
                self.winearch_selection = Some(arch);
                self.config.winearch = match arch {
                    WineArch::Auto => None,
                    WineArch::Win32 => Some("win32".to_string()),
                    WineArch::Win64 => Some("win64".to_string()),
                };
            }
            Message::ForceToggled(value) => {
                self.config.force = value;
            }
            Message::UnattendedToggled(value) => {
                self.config.unattended = value;
            }
            Message::TorifyToggled(value) => {
                self.config.torify = value;
            }
            Message::IsolateToggled(value) => {
                self.config.isolate = value;
            }
            Message::NoCleanToggled(value) => {
                self.config.no_clean = value;
            }
            Message::VerbosityChanged(level) => {
                self.config.verbosity = level;
            }
        }
    }

    fn view(&self) -> Element<'_, Message> {
        container(row![self.sidebar(), self.content()].spacing(0))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(iced::theme::Container::Custom(Box::new(
                BackgroundContainerStyle,
            )))
            .into()
    }

    fn theme(&self) -> Theme {
        Theme::Dark
    }
}

impl WinetricksApp {
    fn sidebar(&self) -> Element<'_, Message> {
        let is_browse = self.current_view == View::Browse;
        let is_installed = self.current_view == View::Installed;
        let is_prefs = self.current_view == View::Preferences;
        let is_tools = self.current_view == View::WineTools;

        let browse_btn =
            self.sidebar_button("Browse", is_browse, Message::ViewChanged(View::Browse));
        let installed_btn = self.sidebar_button(
            "Installed",
            is_installed,
            Message::ViewChanged(View::Installed),
        );
        let prefs_btn = self.sidebar_button(
            "Preferences",
            is_prefs,
            Message::ViewChanged(View::Preferences),
        );
        let tools_btn = self.sidebar_button(
            "Wine Tools",
            is_tools,
            Message::ViewChanged(View::WineTools),
        );

        container(
            column![
                container(
                    text("Winetricks")
                        .size(22)
                        .style(iced::theme::Text::Color(colors::TEXT_PRIMARY))
                )
                .padding([20, 16, 24, 16]),
                browse_btn,
                installed_btn,
                prefs_btn,
                tools_btn,
            ]
            .spacing(4),
        )
        .width(Length::Fixed(200.0))
        .height(Length::Fill)
        .style(iced::theme::Container::Custom(Box::new(
            SidebarContainerStyle,
        )))
        .into()
    }

    fn sidebar_button<'a>(&self, label: &str, active: bool, msg: Message) -> Element<'a, Message> {
        container(
            button(
                text(label)
                    .size(14)
                    .style(iced::theme::Text::Color(if active {
                        colors::PRIMARY
                    } else {
                        colors::TEXT_SECONDARY
                    })),
            )
            .width(Length::Fill)
            .padding([12, 16])
            .style(iced::theme::Button::Custom(Box::new(SidebarButtonStyle {
                _active: active,
            })))
            .on_press(msg),
        )
        .padding([0, 8])
        .into()
    }

    fn content(&self) -> Element<'_, Message> {
        container(match self.current_view {
            View::Browse => self.browse_view(),
            View::Installed => self.installed_view(),
            View::Preferences => self.preferences_view(),
            View::WineTools => self.wine_tools_view(),
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(24)
        .into()
    }

    fn browse_view(&self) -> Element<'_, Message> {
        let search_bar = text_input("Search verbs...", &self.search_query)
            .on_input(Message::SearchChanged)
            .padding(12)
            .style(iced::theme::TextInput::Custom(Box::new(SearchInputStyle)));

        // Category buttons
        let all_active = self.selected_category.is_none();
        let apps_active = self.selected_category == Some(VerbCategory::Apps);
        let dlls_active = self.selected_category == Some(VerbCategory::Dlls);
        let fonts_active = self.selected_category == Some(VerbCategory::Fonts);
        let settings_active = self.selected_category == Some(VerbCategory::Settings);

        let all_btn = self.category_button("All", all_active, None);
        let apps_btn = self.category_button("Apps", apps_active, Some(VerbCategory::Apps));
        let dlls_btn = self.category_button("DLLs", dlls_active, Some(VerbCategory::Dlls));
        let fonts_btn = self.category_button("Fonts", fonts_active, Some(VerbCategory::Fonts));
        let settings_btn =
            self.category_button("Settings", settings_active, Some(VerbCategory::Settings));

        let category_row = row![all_btn, apps_btn, dlls_btn, fonts_btn, settings_btn].spacing(8);

        // Verb list
        let verbs: Vec<_> = if let Some(category) = self.selected_category {
            self.registry
                .list_by_category(category)
                .into_iter()
                .filter(|v| {
                    self.search_query.is_empty()
                        || v.name.contains(&self.search_query)
                        || v.title
                            .to_lowercase()
                            .contains(&self.search_query.to_lowercase())
                })
                .collect()
        } else {
            // Show all verbs
            [
                VerbCategory::Apps,
                VerbCategory::Dlls,
                VerbCategory::Fonts,
                VerbCategory::Settings,
                VerbCategory::Benchmarks,
            ]
            .iter()
            .flat_map(|cat| self.registry.list_by_category(*cat))
            .filter(|v| {
                self.search_query.is_empty()
                    || v.name.contains(&self.search_query)
                    || v.title
                        .to_lowercase()
                        .contains(&self.search_query.to_lowercase())
            })
            .take(100)
            .collect()
        };

        let verb_list: Vec<Element<Message>> = verbs
            .iter()
            .map(|verb| {
                let is_installed = self.installed_verbs.contains(&verb.name);
                let action_msg = if is_installed {
                    Message::UninstallVerb(verb.name.clone())
                } else {
                    Message::InstallVerb(verb.name.clone())
                };

                container(
                    row![
                        column![
                            text(&verb.title)
                                .size(16)
                                .style(iced::theme::Text::Color(colors::TEXT_PRIMARY)),
                            if let Some(ref desc) = verb.publisher {
                                text(desc)
                                    .size(12)
                                    .style(iced::theme::Text::Color(colors::TEXT_DIM))
                            } else {
                                text("")
                                    .size(12)
                                    .style(iced::theme::Text::Color(colors::TEXT_DIM))
                            }
                        ]
                        .spacing(4)
                        .width(Length::Fill),
                        self.action_button(
                            if is_installed { "Uninstall" } else { "Install" },
                            !is_installed,
                            action_msg
                        )
                    ]
                    .spacing(16)
                    .align_items(Alignment::Center)
                    .padding(16),
                )
                .style(iced::theme::Container::Custom(Box::new(CardContainerStyle)))
                .into()
            })
            .collect();

        scrollable(
            column![
                text("Browse Verbs")
                    .size(32)
                    .style(iced::theme::Text::Color(colors::TEXT_PRIMARY)),
                text("Search and install applications, DLLs, fonts, and more")
                    .size(14)
                    .style(iced::theme::Text::Color(colors::TEXT_SECONDARY)),
                search_bar.width(Length::Fill),
                category_row,
                column(verb_list).spacing(8),
            ]
            .spacing(20)
            .width(Length::Fill),
        )
        .into()
    }

    fn category_button<'a>(
        &self,
        label: &str,
        active: bool,
        category: Option<VerbCategory>,
    ) -> Element<'a, Message> {
        let msg = category.map(Message::CategorySelected);

        container(
            button(
                text(label)
                    .size(13)
                    .style(iced::theme::Text::Color(if active {
                        Color::BLACK
                    } else {
                        colors::TEXT_SECONDARY
                    })),
            )
            .padding([8, 16])
            .style(iced::theme::Button::Custom(Box::new(CategoryButtonStyle {
                _active: active,
            })))
            .on_press_maybe(msg),
        )
        .style(iced::theme::Container::Custom(Box::new(
            CategoryContainerStyle { active },
        )))
        .into()
    }

    fn action_button<'a>(&self, label: &str, primary: bool, msg: Message) -> Element<'a, Message> {
        button(
            text(label)
                .size(13)
                .style(iced::theme::Text::Color(if primary {
                    Color::WHITE
                } else {
                    colors::TEXT_SECONDARY
                })),
        )
        .padding([10, 20])
        .style(iced::theme::Button::Custom(Box::new(ActionButtonStyle {
            primary,
        })))
        .on_press(msg)
        .into()
    }

    fn installed_view(&self) -> Element<'_, Message> {
        let installed_list: Vec<Element<Message>> = self
            .installed_verbs
            .iter()
            .map(|verb_name| {
                container(
                    row![
                        text(verb_name)
                            .size(16)
                            .style(iced::theme::Text::Color(colors::TEXT_PRIMARY))
                            .width(Length::Fill),
                        self.action_button(
                            "Uninstall",
                            false,
                            Message::UninstallVerb(verb_name.clone())
                        )
                    ]
                    .spacing(16)
                    .align_items(Alignment::Center)
                    .padding(16),
                )
                .style(iced::theme::Container::Custom(Box::new(CardContainerStyle)))
                .into()
            })
            .collect();

        scrollable(
            column![
                text("Installed Verbs")
                    .size(32)
                    .style(iced::theme::Text::Color(colors::TEXT_PRIMARY)),
                text("View and manage installed verbs in your wineprefix")
                    .size(14)
                    .style(iced::theme::Text::Color(colors::TEXT_SECONDARY)),
                column(installed_list).spacing(8),
            ]
            .spacing(20),
        )
        .into()
    }

    fn preferences_view(&self) -> Element<'_, Message> {
        scrollable(
            column![
                text("Preferences")
                    .size(32)
                    .style(iced::theme::Text::Color(colors::TEXT_PRIMARY)),
                text("Configure Winetricks settings and options")
                    .size(14)
                    .style(iced::theme::Text::Color(colors::TEXT_SECONDARY)),
                // Wine Prefix Section
                self.settings_section(
                    "Wine Prefix",
                    "Configure Wine prefix and architecture settings",
                    column![
                        self.setting_row(
                            "Wineprefix Path",
                            "Location of the Wine prefix directory",
                            row![
                                text_input("e.g., ~/.wine", &self.wineprefix_input)
                                    .on_input(Message::WineprefixChanged)
                                    .padding(10)
                                    .width(Length::Fill),
                                button(
                                    text("Browse")
                                        .size(13)
                                        .style(iced::theme::Text::Color(Color::WHITE))
                                )
                                .padding([10, 16])
                                .style(iced::theme::Button::Custom(Box::new(ActionButtonStyle {
                                    primary: true
                                })))
                                .on_press(Message::BrowseWineprefix)
                            ]
                            .spacing(8)
                            .width(Length::Fill)
                            .into()
                        ),
                        self.setting_row(
                            "Architecture",
                            "Wine architecture (win32 or win64)",
                            pick_list(
                                WineArch::all(),
                                self.winearch_selection,
                                Message::WinearchChanged
                            )
                            .padding(10)
                            .width(Length::Fill)
                            .into()
                        ),
                    ]
                    .into()
                ),
                // Installation Options Section
                self.settings_section(
                    "Installation Options",
                    "Configure how verbs are installed",
                    column![
                        self.checkbox_row(
                            "Force Reinstall",
                            "Don't check if packages are already installed (-f, --force)",
                            self.config.force,
                            Message::ForceToggled
                        ),
                        self.checkbox_row(
                            "Unattended Mode",
                            "Don't ask questions, install automatically (-q, --unattended)",
                            self.config.unattended,
                            Message::UnattendedToggled
                        ),
                        self.checkbox_row(
                            "Isolate Prefixes",
                            "Install each app in its own WINEPREFIX (--isolate)",
                            self.config.isolate,
                            Message::IsolateToggled
                        ),
                        self.checkbox_row(
                            "Keep Temp Directories",
                            "Don't delete temp directories (--no-clean, useful for debugging)",
                            self.config.no_clean,
                            Message::NoCleanToggled
                        ),
                    ]
                    .into()
                ),
                // Network Options Section
                self.settings_section(
                    "Network Options",
                    "Configure download and network settings",
                    column![
                        self.checkbox_row(
                            "Use Torify",
                            "Run downloads under torify, if available (-t, --torify)",
                            self.config.torify,
                            Message::TorifyToggled
                        ),
                        self.setting_row(
                            "Country Code",
                            "Set country code to CC (--country=CC)",
                            text_input("e.g., US, GB, DE", &self.country_input)
                                .on_input(Message::CountryChanged)
                                .padding(10)
                                .width(Length::Fixed(100.0))
                                .into()
                        ),
                    ]
                    .into()
                ),
                // Verbosity Section
                self.settings_section(
                    "Verbosity",
                    "Configure output verbosity levels",
                    column![
                        self.checkbox_row(
                            "Verbose",
                            "Echo all commands as they are executed (-v, --verbose)",
                            self.config.verbosity >= 1,
                            |v| Message::VerbosityChanged(if v { 1 } else { 0 })
                        ),
                        self.checkbox_row(
                            "Really Verbose",
                            "Really verbose mode (-vv, --really-verbose)",
                            self.config.verbosity >= 2,
                            |v| Message::VerbosityChanged(if v { 2 } else { 0 })
                        ),
                    ]
                    .into()
                ),
                // Information Section
                self.settings_section(
                    "Information",
                    "Paths and directories used by Winetricks",
                    column![
                        self.info_row("Cache Directory", &self.config.cache_dir.to_string_lossy()),
                        self.info_row("Data Directory", &self.config.data_dir.to_string_lossy()),
                        self.info_row(
                            "Prefixes Root",
                            &self.config.prefixes_root.to_string_lossy()
                        ),
                    ]
                    .into()
                ),
            ]
            .spacing(20)
            .width(Length::Fill),
        )
        .into()
    }

    fn settings_section<'a>(
        &self,
        title: &str,
        description: &str,
        content: Element<'a, Message>,
    ) -> Element<'a, Message> {
        container(
            column![
                text(title)
                    .size(20)
                    .style(iced::theme::Text::Color(colors::TEXT_PRIMARY)),
                text(description)
                    .size(13)
                    .style(iced::theme::Text::Color(colors::TEXT_SECONDARY)),
                container(content).padding(16)
            ]
            .spacing(12)
            .padding(20),
        )
        .style(iced::theme::Container::Custom(Box::new(CardContainerStyle)))
        .into()
    }

    fn setting_row<'a>(
        &self,
        title: &str,
        description: &str,
        control: Element<'a, Message>,
    ) -> Element<'a, Message> {
        row![
            column![
                text(title)
                    .size(15)
                    .style(iced::theme::Text::Color(colors::TEXT_PRIMARY)),
                text(description)
                    .size(12)
                    .style(iced::theme::Text::Color(colors::TEXT_SECONDARY)),
            ]
            .spacing(4)
            .width(Length::Fill),
            container(control).width(Length::Fixed(300.0))
        ]
        .spacing(16)
        .align_items(Alignment::Center)
        .padding([12, 0])
        .into()
    }

    fn checkbox_row(
        &self,
        title: &str,
        description: &str,
        checked: bool,
        msg_fn: fn(bool) -> Message,
    ) -> Element<'_, Message> {
        row![
            column![
                text(title)
                    .size(15)
                    .style(iced::theme::Text::Color(colors::TEXT_PRIMARY)),
                text(description)
                    .size(12)
                    .style(iced::theme::Text::Color(colors::TEXT_SECONDARY)),
            ]
            .spacing(4)
            .width(Length::Fill),
            checkbox("", checked).text_size(14).on_toggle(msg_fn)
        ]
        .spacing(16)
        .align_items(Alignment::Center)
        .padding([12, 0])
        .into()
    }

    fn info_row<'a>(&self, title: &str, value: &str) -> Element<'a, Message> {
        row![
            text(title)
                .size(14)
                .style(iced::theme::Text::Color(colors::TEXT_SECONDARY))
                .width(Length::Fill),
            text(value)
                .size(13)
                .style(iced::theme::Text::Color(colors::TEXT_DIM)),
        ]
        .spacing(16)
        .padding([8, 0])
        .into()
    }

    fn wine_tools_view(&self) -> Element<'_, Message> {
        scrollable(
            column![
                text("Wine Tools")
                    .size(32)
                    .style(iced::theme::Text::Color(colors::TEXT_PRIMARY)),
                text("Quick access to Wine utilities")
                    .size(14)
                    .style(iced::theme::Text::Color(colors::TEXT_SECONDARY)),
                self.tool_card(
                    "Wine Configuration",
                    "Configure Wine settings, libraries, and applications",
                    "winecfg"
                ),
                self.tool_card(
                    "Registry Editor",
                    "Edit Windows registry entries",
                    "regedit"
                ),
                self.tool_card(
                    "Task Manager",
                    "View running processes and applications",
                    "taskmgr"
                ),
                self.tool_card(
                    "File Explorer",
                    "Browse Wine filesystem in Windows explorer",
                    "explorer"
                ),
                self.tool_card("Uninstaller", "Manage installed programs", "uninstaller"),
                self.tool_card(
                    "Wine Shell",
                    "Open interactive shell with Wine environment",
                    "shell"
                ),
                self.tool_card(
                    "Command Prompt",
                    "Open Windows command prompt (cmd.exe)",
                    "winecmd"
                ),
                self.tool_card(
                    "Open Prefix Folder",
                    "Open WINEPREFIX directory in file manager",
                    "folder"
                ),
                self.tool_card(
                    "Winetricks Wiki",
                    "Open Winetricks documentation in browser",
                    "help"
                ),
            ]
            .spacing(12),
        )
        .into()
    }

    fn tool_card<'a>(&self, title: &str, description: &str, tool: &str) -> Element<'a, Message> {
        container(
            button(
                column![
                    text(title)
                        .size(16)
                        .style(iced::theme::Text::Color(colors::TEXT_PRIMARY)),
                    text(description)
                        .size(13)
                        .style(iced::theme::Text::Color(colors::TEXT_SECONDARY)),
                ]
                .spacing(4)
                .align_items(Alignment::Start),
            )
            .width(Length::Fill)
            .padding(20)
            .style(iced::theme::Button::Custom(Box::new(ToolButtonStyle)))
            .on_press(Message::RunWineTool(tool.to_string())),
        )
        .style(iced::theme::Container::Custom(Box::new(CardContainerStyle)))
        .into()
    }
}

// Custom container styles
struct BackgroundContainerStyle;

impl container::StyleSheet for BackgroundContainerStyle {
    type Style = iced::Theme;

    fn appearance(&self, _style: &Self::Style) -> container::Appearance {
        container::Appearance {
            background: Some(colors::BACKGROUND.into()),
            ..Default::default()
        }
    }
}

struct SidebarContainerStyle;

impl container::StyleSheet for SidebarContainerStyle {
    type Style = iced::Theme;

    fn appearance(&self, _style: &Self::Style) -> container::Appearance {
        container::Appearance {
            background: Some(colors::SURFACE.into()),
            ..Default::default()
        }
    }
}

struct CardContainerStyle;

impl container::StyleSheet for CardContainerStyle {
    type Style = iced::Theme;

    fn appearance(&self, _style: &Self::Style) -> container::Appearance {
        container::Appearance {
            background: Some(colors::SURFACE.into()),
            border: iced::Border::with_radius(12.0),
            ..Default::default()
        }
    }
}

struct CategoryContainerStyle {
    active: bool,
}

impl container::StyleSheet for CategoryContainerStyle {
    type Style = iced::Theme;

    fn appearance(&self, _style: &Self::Style) -> container::Appearance {
        container::Appearance {
            background: Some(
                if self.active {
                    colors::PRIMARY
                } else {
                    colors::SURFACE
                }
                .into(),
            ),
            border: iced::Border::with_radius(8.0),
            ..Default::default()
        }
    }
}

// Custom button styles
#[allow(dead_code)]
struct SidebarButtonStyle {
    _active: bool,
}

impl button::StyleSheet for SidebarButtonStyle {
    type Style = iced::Theme;

    fn active(&self, _style: &Self::Style) -> button::Appearance {
        button::Appearance {
            background: None,
            border: iced::Border::with_radius(8.0),
            ..Default::default()
        }
    }

    fn hovered(&self, _style: &Self::Style) -> button::Appearance {
        button::Appearance {
            background: Some(colors::SURFACE_HOVER.into()),
            border: iced::Border::with_radius(8.0),
            ..Default::default()
        }
    }
}

#[allow(dead_code)]
struct CategoryButtonStyle {
    _active: bool,
}

impl button::StyleSheet for CategoryButtonStyle {
    type Style = iced::Theme;

    fn active(&self, _style: &Self::Style) -> button::Appearance {
        button::Appearance {
            background: None,
            border: iced::Border::with_radius(8.0),
            ..Default::default()
        }
    }
}

struct ActionButtonStyle {
    primary: bool,
}

impl button::StyleSheet for ActionButtonStyle {
    type Style = iced::Theme;

    fn active(&self, _style: &Self::Style) -> button::Appearance {
        button::Appearance {
            background: Some(
                if self.primary {
                    colors::PRIMARY
                } else {
                    colors::SURFACE
                }
                .into(),
            ),
            border: iced::Border::with_radius(8.0),
            ..Default::default()
        }
    }

    fn hovered(&self, _style: &Self::Style) -> button::Appearance {
        button::Appearance {
            background: Some(
                if self.primary {
                    colors::PRIMARY_HOVER
                } else {
                    colors::SURFACE_HOVER
                }
                .into(),
            ),
            border: iced::Border::with_radius(8.0),
            ..Default::default()
        }
    }
}

struct ToolButtonStyle;

impl button::StyleSheet for ToolButtonStyle {
    type Style = iced::Theme;

    fn active(&self, _style: &Self::Style) -> button::Appearance {
        button::Appearance {
            background: None,
            border: iced::Border::with_radius(0.0),
            ..Default::default()
        }
    }

    fn hovered(&self, _style: &Self::Style) -> button::Appearance {
        button::Appearance {
            background: Some(colors::SURFACE_HOVER.into()),
            border: iced::Border::with_radius(0.0),
            ..Default::default()
        }
    }
}

struct SearchInputStyle;

impl text_input::StyleSheet for SearchInputStyle {
    type Style = iced::Theme;

    fn active(&self, _style: &Self::Style) -> text_input::Appearance {
        text_input::Appearance {
            background: colors::SURFACE.into(),
            border: iced::Border::with_radius(10.0),
            icon_color: colors::TEXT_DIM,
        }
    }

    fn focused(&self, _style: &Self::Style) -> text_input::Appearance {
        text_input::Appearance {
            background: colors::SURFACE.into(),
            border: iced::Border::with_radius(10.0),
            icon_color: colors::PRIMARY,
        }
    }

    fn placeholder_color(&self, _style: &Self::Style) -> Color {
        colors::TEXT_DIM
    }

    fn value_color(&self, _style: &Self::Style) -> Color {
        colors::TEXT_PRIMARY
    }

    fn disabled_color(&self, _style: &Self::Style) -> Color {
        colors::TEXT_DIM
    }

    fn selection_color(&self, _style: &Self::Style) -> Color {
        colors::PRIMARY
    }

    fn disabled(&self, _style: &Self::Style) -> text_input::Appearance {
        text_input::Appearance {
            background: colors::SURFACE.into(),
            border: iced::Border::with_radius(10.0),
            icon_color: colors::TEXT_DIM,
        }
    }
}

fn run_wine_tool(config: &Config, tool: &str) {
    use std::process;
    use winetricks_lib::Wine;

    let wine = match Wine::detect() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("Error: Could not detect Wine: {}", e);
            return;
        }
    };

    let wineprefix = config.wineprefix();

    match tool {
        "winecfg" => {
            let _ = process::Command::new(&wine.wine_bin)
                .arg("winecfg")
                .env("WINEPREFIX", &wineprefix)
                .spawn();
        }
        "regedit" => {
            let mut cmd = process::Command::new(&wine.wine_bin);
            cmd.arg("regedit").env("WINEPREFIX", &wineprefix);

            if config.unattended {
                cmd.arg("/S"); // Silent mode
            }

            let _ = cmd.spawn();
        }
        "taskmgr" => {
            let _ = process::Command::new(&wine.wine_bin)
                .arg("taskmgr")
                .env("WINEPREFIX", &wineprefix)
                .spawn();
        }
        "explorer" => {
            let _ = process::Command::new(&wine.wine_bin)
                .arg("explorer")
                .env("WINEPREFIX", &wineprefix)
                .spawn();
        }
        "uninstaller" => {
            let _ = process::Command::new(&wine.wine_bin)
                .arg("uninstaller")
                .env("WINEPREFIX", &wineprefix)
                .spawn();
        }
        "shell" => {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
            let wine_path = wine.wine_bin.to_string_lossy().to_string();
            let prefix_path = wineprefix.to_string_lossy().to_string();

            let terminals = [
                "gnome-terminal",
                "konsole",
                "xterm",
                "mate-terminal",
                "terminator",
            ];
            let term = terminals.iter().find(|t| which::which(*t).is_ok());

            if let Some(term) = term {
                let term_args = match *term {
                    "gnome-terminal" => vec!["--".to_string(), shell.clone()],
                    _ => vec!["-e".to_string(), shell.clone()],
                };

                let _ = process::Command::new(*term)
                    .args(term_args)
                    .env("WINEPREFIX", &prefix_path)
                    .env("WINE", &wine_path)
                    .spawn();
            } else {
                // Fall back to direct shell
                let mut cmd = process::Command::new(&shell);
                cmd.env("WINEPREFIX", &wineprefix)
                    .env("WINE", &wine.wine_bin)
                    .env("WINEDEBUG", "-all");
                let _ = cmd.spawn();
            }
        }
        "winecmd" => {
            let _ = process::Command::new(&wine.wine_bin)
                .arg("cmd")
                .env("WINEPREFIX", &wineprefix)
                .spawn();
        }
        "folder" => {
            let file_managers = ["xdg-open", "open", "cygstart"];
            for fm in &file_managers {
                if process::Command::new(fm).arg(&wineprefix).spawn().is_ok() {
                    return;
                }
            }
            eprintln!(
                "Could not open file manager. Please open: {}",
                wineprefix.display()
            );
        }
        "help" => {
            let url = "https://github.com/Winetricks/winetricks/wiki";
            let browsers = ["xdg-open", "sdtwebclient", "cygstart", "open", "firefox"];

            for browser in &browsers {
                if process::Command::new(browser).arg(url).spawn().is_ok() {
                    return;
                }
            }
            eprintln!("Could not open browser. Please visit: {}", url);
        }
        _ => {
            eprintln!("Unknown tool: {}", tool);
        }
    }
}

fn load_installed_verbs(config: &Config) -> Vec<String> {
    let log_file = config.wineprefix().join("winetricks.log");

    if !log_file.exists() {
        return Vec::new();
    }

    std::fs::read_to_string(&log_file)
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with('-')
                && !l.starts_with('#')
                && !l.starts_with("//")
                && !l.contains('=')
                && !matches!(l.as_str(), "list" | "list-installed" | "list-all")
        })
        .collect()
}
