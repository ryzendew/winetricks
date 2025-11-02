//! Winetricks Cosmic GUI
//!
//! COSMIC desktop-optimized GUI built with libcosmic

#[cfg(feature = "cosmic")]
pub mod cosmic_impl {
    use cosmic::app::{Command, Core, Settings};
    use cosmic::iced::widget::{
        button, checkbox, column, container, pick_list, row, scrollable, text, text_input,
    };
    use cosmic::iced::{Alignment, Element, Length, Renderer};
    use cosmic::{ApplicationExt, Element as CosmicElement};

    // Helper macros since cosmic wraps iced
    macro_rules! column {
        ($($item:expr),* $(,)?) => {
            column(vec![$($item.into()),*])
        };
    }

    macro_rules! row {
        ($($item:expr),* $(,)?) => {
            row(vec![$($item.into()),*])
        };
    }
    use std::path::PathBuf;
    use winetricks_lib::{Config, VerbCategory, VerbRegistry};

    // Re-export types for consistency
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum View {
        Browse,
        Installed,
        Preferences,
        WineTools,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum WineArch {
        Auto,
        Win32,
        Win64,
    }

    impl WineArch {
        pub fn all() -> [WineArch; 3] {
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

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Renderer {
        Auto,
        OpenGL,
        Vulkan,
    }

    impl Renderer {
        pub fn all() -> [Renderer; 3] {
            [Renderer::Auto, Renderer::OpenGL, Renderer::Vulkan]
        }
    }

    impl std::fmt::Display for Renderer {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Renderer::Auto => write!(f, "Auto (default)"),
                Renderer::OpenGL => write!(f, "OpenGL (gl)"),
                Renderer::Vulkan => write!(f, "Vulkan"),
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum WaylandDisplay {
        Auto,
        Wayland,
        XWayland,
    }

    impl WaylandDisplay {
        pub fn all() -> [WaylandDisplay; 3] {
            [WaylandDisplay::Auto, WaylandDisplay::Wayland, WaylandDisplay::XWayland]
        }
    }

    impl std::fmt::Display for WaylandDisplay {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                WaylandDisplay::Auto => write!(f, "Auto (detect)"),
                WaylandDisplay::Wayland => write!(f, "Wayland (native)"),
                WaylandDisplay::XWayland => write!(f, "XWayland (X11)"),
            }
        }
    }

    #[derive(Debug, Clone)]
    pub enum Message {
        ViewChanged(View),
        SearchChanged(String),
        CategorySelected(VerbCategory),
        InstallVerb(String),
        UninstallVerb(String),
        RunWineTool(String),
        WineprefixChanged(String),
        BrowseWineprefix,
        CountryChanged(String),
        WinearchChanged(WineArch),
        RendererChanged(Renderer),
        WaylandChanged(WaylandDisplay),
        ForceToggled(bool),
        UnattendedToggled(bool),
        TorifyToggled(bool),
        IsolateToggled(bool),
        NoCleanToggled(bool),
        VerbosityChanged(u8),
        OperationStatusUpdate(Option<OperationStatus>),
    }
    
    #[derive(Debug, Clone)]
    enum OperationStatus {
        Uninstalling { verb_name: String },
        Installing { verb_name: String },
    }

    pub struct WinetricksCosmicApp {
        core: Core,
        config: Config,
        registry: VerbRegistry,
        current_view: View,
        search_query: String,
        selected_category: Option<VerbCategory>,
        installed_verbs: Vec<String>,
        wineprefix_input: String,
        country_input: String,
        winearch_selection: Option<WineArch>,
        renderer_selection: Option<Renderer>,
        wayland_selection: Option<WaylandDisplay>,
        operation_status: Option<OperationStatus>,
    }

    impl cosmic::Application for WinetricksCosmicApp {
        type Executor = cosmic::DefaultExecutor;
        type Flags = ();
        type Message = Message;
        type Theme = cosmic::Theme;

        fn core(&self) -> &Core {
            &self.core
        }

        fn core_mut(&mut self) -> &mut Core {
            &mut self.core
        }

        fn init(core: Core, _flags: Self::Flags) -> (Self, Command<Self::Message>) {
            let config = Config::new().unwrap_or_else(|_| Config::default());
            let metadata_dir = config.metadata_dir();

            let registry = if metadata_dir.exists() {
                VerbRegistry::load_from_dir(metadata_dir).unwrap_or_else(|_| VerbRegistry::new())
            } else {
                VerbRegistry::new()
            };

            let installed_verbs = load_installed_verbs(&config);
            let wineprefix_input = config.wineprefix().to_string_lossy().to_string();
            let winearch_selection = match config.winearch.as_deref() {
                Some("win32") => Some(WineArch::Win32),
                Some("win64") => Some(WineArch::Win64),
                _ => Some(WineArch::Auto),
            };
            
            // Load renderer from wineprefix registry
            config.load_renderer_from_prefix();
            let renderer_selection = config.renderer.as_ref().and_then(|renderer| {
                match renderer.to_lowercase().as_str() {
                    "opengl" | "gl" => Some(Renderer::OpenGL),
                    "vulkan" | "vk" | "v" => Some(Renderer::Vulkan),
                    _ => None,
                }
            });
            
            // Load wayland setting from wineprefix registry (with env fallback for initial load)
            config.load_wayland_from_prefix_with_env();
            let wayland_selection = config.wayland.as_ref().and_then(|wayland| {
                match wayland.to_lowercase().as_str() {
                    "wayland" => Some(WaylandDisplay::Wayland),
                    "xwayland" | "x11" => Some(WaylandDisplay::XWayland),
                    _ => None,
                }
            });

            (
                Self {
                    core,
                    config,
                    registry,
                    current_view: View::Browse,
                    search_query: String::new(),
                    selected_category: None,
                    installed_verbs,
                    wineprefix_input,
                    country_input: String::new(),
                    winearch_selection,
                    renderer_selection,
                    wayland_selection,
                    operation_status: None,
                },
                Command::none(),
            )
        }

        fn update(&mut self, message: Self::Message) -> Command<Self::Message> {
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
                    eprintln!("Uninstalling verb: {}", verb_name);
                    // Show progress dialog
                    self.operation_status = Some(OperationStatus::Uninstalling { 
                        verb_name: verb_name.clone() 
                    });
                    
                    // Spawn async task to uninstall
                    let config = self.config.clone();
                    let verb_name_clone = verb_name.clone();
                    let config_for_reload = self.config.clone();
                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Runtime::new().unwrap();
                        rt.block_on(async {
                            match winetricks_lib::Executor::new(config.clone()).await {
                                Ok(mut executor) => {
                                    match executor.uninstall_verb(&verb_name_clone).await {
                                        Ok(_) => {
                                            eprintln!("Successfully uninstalled: {}", verb_name_clone);
                                            // Reload installed verbs list after successful uninstall
                                            let updated_verbs = load_installed_verbs(&config_for_reload);
                                            eprintln!("Remaining installed verbs: {:?}", updated_verbs);
                                        }
                                        Err(e) => {
                                            eprintln!("Error uninstalling {}: {}", verb_name_clone, e);
                                        }
                                    }
                                }
                                Err(e) => {
                                    eprintln!("Error creating executor: {}", e);
                                }
                            }
                        });
                    });
                    // Optimistically remove from UI list immediately for instant feedback
                    self.installed_verbs.retain(|v| v != &verb_name);
                    
                    // Auto-close dialog after operation completes
                    let verb_name_for_close = verb_name.clone();
                    let config_for_check = self.config.clone();
                    std::thread::spawn(move || {
                        // Poll every 200ms for up to 10 seconds
                        for i in 0..50 {
                            std::thread::sleep(std::time::Duration::from_millis(200));
                            let updated = load_installed_verbs(&config_for_check);
                            if !updated.contains(&verb_name_for_close) {
                                eprintln!("Uninstall completed - closing dialog (after {}ms)", i * 200);
                                break;
                            }
                        }
                    });
                    return Command::none();
                }
                Message::OperationStatusUpdate(status) => {
                    let completed = status.is_none();
                    self.operation_status = status;
                    if completed {
                        self.installed_verbs = load_installed_verbs(&self.config);
                    }
                    return Command::none();
                }
                Message::RunWineTool(tool) => {
                    eprintln!("Running Wine tool: {}", tool);
                    run_wine_tool(&self.config, &tool);
                }
                Message::WineprefixChanged(value) => {
                    self.wineprefix_input = value.clone();
                    let new_path = if let Ok(path) = PathBuf::from(&value).canonicalize() {
                        path
                    } else {
                        PathBuf::from(&value)
                    };
                    self.config.wineprefix = Some(new_path);
                    self.installed_verbs = load_installed_verbs(&self.config);
                }
                Message::BrowseWineprefix => {
                    let current_prefix = PathBuf::from(&self.wineprefix_input);
                    let start_dir = if current_prefix.exists() {
                        current_prefix.clone()
                    } else {
                        current_prefix
                            .parent()
                            .map(|p| p.to_path_buf())
                            .unwrap_or_else(|| {
                                dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
                            })
                    };

                    let start_path = start_dir.to_string_lossy().to_string();

                    // Try zenity
                    if let Ok(output) = std::process::Command::new("zenity")
                        .arg("--file-selection")
                        .arg("--directory")
                        .arg("--title=Select Wineprefix Directory")
                        .arg(format!("--filename={}", start_path))
                        .output()
                    {
                        if output.status.success() {
                            let selected =
                                String::from_utf8_lossy(&output.stdout).trim().to_string();
                            if !selected.is_empty() {
                                self.wineprefix_input = selected.clone();
                                let new_path =
                                    if let Ok(path) = PathBuf::from(&selected).canonicalize() {
                                        path
                                    } else {
                                        PathBuf::from(&selected)
                                    };
                    self.config.wineprefix = Some(new_path);
                    
                    // Load renderer from new wineprefix
                    self.config.load_renderer_from_prefix();
                    self.renderer_selection = self.config.renderer.as_ref().and_then(|renderer| {
                        match renderer.to_lowercase().as_str() {
                            "opengl" | "gl" => Some(Renderer::OpenGL),
                            "vulkan" | "vk" | "v" => Some(Renderer::Vulkan),
                            _ => None,
                        }
                    });
                    
                    // Load wayland setting from new wineprefix
                    self.config.load_wayland_from_prefix();
                    self.wayland_selection = self.config.wayland.as_ref().and_then(|wayland| {
                        match wayland.to_lowercase().as_str() {
                            "wayland" => Some(WaylandDisplay::Wayland),
                            "xwayland" | "x11" => Some(WaylandDisplay::XWayland),
                            _ => None,
                        }
                    });
                    
                    self.installed_verbs = load_installed_verbs(&self.config);
                                    return Command::none();
                            }
                        }
                    }

                    // Try kdialog
                    if let Ok(output) = std::process::Command::new("kdialog")
                        .arg("--getexistingdirectory")
                        .arg(&start_path)
                        .arg("--title")
                        .arg("Select Wineprefix Directory")
                        .output()
                    {
                        if output.status.success() {
                            let selected =
                                String::from_utf8_lossy(&output.stdout).trim().to_string();
                            if !selected.is_empty() && !selected.starts_with("Error:") {
                                self.wineprefix_input = selected.clone();
                                let new_path =
                                    if let Ok(path) = PathBuf::from(&selected).canonicalize() {
                                        path
                                    } else {
                                        PathBuf::from(&selected)
                                    };
                    self.config.wineprefix = Some(new_path);
                    
                    // Load renderer from new wineprefix
                    self.config.load_renderer_from_prefix();
                    self.renderer_selection = self.config.renderer.as_ref().and_then(|renderer| {
                        match renderer.to_lowercase().as_str() {
                            "opengl" | "gl" => Some(Renderer::OpenGL),
                            "vulkan" | "vk" | "v" => Some(Renderer::Vulkan),
                            _ => None,
                        }
                    });
                    
                    // Load wayland setting from new wineprefix
                    self.config.load_wayland_from_prefix();
                    self.wayland_selection = self.config.wayland.as_ref().and_then(|wayland| {
                        match wayland.to_lowercase().as_str() {
                            "wayland" => Some(WaylandDisplay::Wayland),
                            "xwayland" | "x11" => Some(WaylandDisplay::XWayland),
                            _ => None,
                        }
                    });
                    
                    self.installed_verbs = load_installed_verbs(&self.config);
                                    return Command::none();
                            }
                        }
                    }

                    // Try yad
                    if let Ok(output) = std::process::Command::new("yad")
                        .arg("--file")
                        .arg("--directory")
                        .arg("--title=Select Wineprefix Directory")
                        .arg(format!("--filename={}", start_path))
                        .output()
                    {
                        if output.status.success() {
                            let selected =
                                String::from_utf8_lossy(&output.stdout).trim().to_string();
                            if !selected.is_empty() {
                                self.wineprefix_input = selected.clone();
                                let new_path =
                                    if let Ok(path) = PathBuf::from(&selected).canonicalize() {
                                        path
                                    } else {
                                        PathBuf::from(&selected)
                                    };
                    self.config.wineprefix = Some(new_path);
                    
                    // Load renderer from new wineprefix
                    self.config.load_renderer_from_prefix();
                    self.renderer_selection = self.config.renderer.as_ref().and_then(|renderer| {
                        match renderer.to_lowercase().as_str() {
                            "opengl" | "gl" => Some(Renderer::OpenGL),
                            "vulkan" | "vk" | "v" => Some(Renderer::Vulkan),
                            _ => None,
                        }
                    });
                    
                    // Load wayland setting from new wineprefix
                    self.config.load_wayland_from_prefix();
                    self.wayland_selection = self.config.wayland.as_ref().and_then(|wayland| {
                        match wayland.to_lowercase().as_str() {
                            "wayland" => Some(WaylandDisplay::Wayland),
                            "xwayland" | "x11" => Some(WaylandDisplay::XWayland),
                            _ => None,
                        }
                    });
                    
                    self.installed_verbs = load_installed_verbs(&self.config);
                                    return Command::none();
                            }
                        }
                    }

                    // Fallback
                    eprintln!(
                        "No file picker found (zenity/kdialog/yad). Opening file manager instead."
                    );
                    let file_managers = ["xdg-open", "open", "cygstart"];
                    for fm in &file_managers {
                        if std::process::Command::new(fm)
                            .arg(&start_dir)
                            .spawn()
                            .is_ok()
                        {
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
                Message::RendererChanged(renderer) => {
                    self.renderer_selection = Some(renderer);
                    let renderer_str = match renderer {
                        Renderer::Auto => None,
                        Renderer::OpenGL => Some("opengl"),
                        Renderer::Vulkan => Some("vulkan"),
                    };
                    self.config.renderer = renderer_str.map(|s| s.to_string());
                    
                    // Set in wineprefix registry for persistence
                    if let Err(e) = self.config.set_renderer_in_registry(renderer_str) {
                        eprintln!("Warning: Failed to set renderer in registry: {}", e);
                    }
                }
                Message::WaylandChanged(wayland) => {
                    self.wayland_selection = Some(wayland);
                    let wayland_str = match wayland {
                        WaylandDisplay::Auto => None,
                        WaylandDisplay::Wayland => Some("wayland"),
                        WaylandDisplay::XWayland => Some("xwayland"),
                    };
                    self.config.wayland = wayland_str.map(|s| s.to_string());
                    
                    // Set or clear in wineprefix registry for persistence
                    if let Err(e) = self.config.set_wayland_in_registry(wayland_str) {
                        eprintln!("Warning: Failed to set Graphics driver in registry: {}", e);
                    } else {
                        // After setting to Auto, clear config.wayland to ensure it shows as Auto
                        if wayland == WaylandDisplay::Auto {
                            self.config.wayland = None;
                        }
                        // Verify registry was updated correctly by reloading (only checks registry, not environment)
                        // Only reload if we set a specific value, not Auto
                        if wayland_str.is_some() {
                            self.config.load_wayland_from_prefix();
                            self.wayland_selection = self.config.wayland.as_ref().and_then(|wayland| {
                                match wayland.to_lowercase().as_str() {
                                    "wayland" => Some(WaylandDisplay::Wayland),
                                    "xwayland" | "x11" => Some(WaylandDisplay::XWayland),
                                    _ => None,
                                }
                            });
                        }
                    }
                    return Command::none();
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
            Command::none()
        }

        fn view(&self) -> CosmicElement<Self::Message> {
            container(
                row(vec![self.sidebar().into(), self.content().into()])
                    .spacing(0)
                    .width(Length::Fill)
                    .height(Length::Fill),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
        }
    }

    impl WinetricksCosmicApp {
        fn sidebar(&self) -> Element<'_, Message> {
            let is_browse = self.current_view == View::Browse;
            let is_installed = self.current_view == View::Installed;
            let is_prefs = self.current_view == View::Preferences;
            let is_tools = self.current_view == View::WineTools;

            container(
                column(vec![
                    container(text("Winetricks").size(22))
                        .padding([20, 16, 24, 16])
                        .into(),
                    self.sidebar_button("Browse", is_browse, Message::ViewChanged(View::Browse)),
                    self.sidebar_button(
                        "Installed",
                        is_installed,
                        Message::ViewChanged(View::Installed),
                    ),
                    self.sidebar_button(
                        "Preferences",
                        is_prefs,
                        Message::ViewChanged(View::Preferences),
                    ),
                    self.sidebar_button(
                        "Wine Tools",
                        is_tools,
                        Message::ViewChanged(View::WineTools),
                    ),
                ])
                .spacing(4),
            )
            .width(Length::Fixed(200.0))
            .height(Length::Fill)
            .into()
        }

        fn sidebar_button<'a>(
            &self,
            label: &str,
            _active: bool,
            msg: Message,
        ) -> Element<'a, Message> {
            button(text(label).size(14))
                .width(Length::Fill)
                .padding([12, 16])
                .on_press(msg)
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
                .padding(12);

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

            let category_row =
                row(vec![all_btn, apps_btn, dlls_btn, fonts_btn, settings_btn]).spacing(8);

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
                        row(vec![
                            column(vec![
                                text(&verb.title).size(16),
                                text(verb.publisher.as_deref().unwrap_or("")).size(12),
                            ])
                            .spacing(4)
                            .width(Length::Fill),
                            self.action_button(
                                if is_installed { "Uninstall" } else { "Install" },
                                !is_installed,
                                action_msg,
                            ),
                        ])
                        .spacing(16)
                        .align_items(Alignment::Center)
                        .padding(16),
                    )
                    .into()
                })
                .collect();

            scrollable(
                column(vec![
                    text("Browse Verbs").size(32),
                    text("Search and install applications, DLLs, fonts, and more").size(14),
                    search_bar.width(Length::Fill),
                    category_row,
                    column(verb_list).spacing(8),
                ])
                .spacing(20)
                .width(Length::Fill),
            )
            .into()
        }

        fn category_button<'a>(
            &self,
            label: &str,
            _active: bool,
            category: Option<VerbCategory>,
        ) -> Element<'a, Message> {
            let msg = category.map(Message::CategorySelected);
            button(text(label).size(13))
                .padding([8, 16])
                .on_press_maybe(msg)
                .into()
        }

        fn action_button<'a>(
            &self,
            label: &str,
            _primary: bool,
            msg: Message,
        ) -> Element<'a, Message> {
            button(text(label).size(13))
                .padding([10, 20])
                .on_press(msg)
                .into()
        }

        fn installed_view(&self) -> Element<'_, Message> {
            let installed_list: Vec<Element<Message>> = self
                .installed_verbs
                .iter()
                .map(|verb_name| {
                    container(
                        row(vec![
                            text(verb_name).size(16).width(Length::Fill),
                            self.action_button(
                                "Uninstall",
                                false,
                                Message::UninstallVerb(verb_name.clone()),
                            ),
                        ])
                        .spacing(16)
                        .align_items(Alignment::Center)
                        .padding(16),
                    )
                    .into()
                })
                .collect();

            scrollable(
                column(vec![
                    text("Installed Verbs").size(32),
                    text("View and manage installed verbs in your wineprefix").size(14),
                    column(installed_list).spacing(8),
                ])
                .spacing(20),
            )
            .into()
        }

        fn preferences_view(&self) -> Element<'_, Message> {
            scrollable(
                column(vec![
                    text("Preferences").size(32),
                    text("Configure Winetricks settings and options").size(14),
                    self.settings_section(
                        "Wine Prefix",
                        "Configure Wine prefix and architecture settings",
                        column(vec![
                            self.setting_row(
                                "Wineprefix Path",
                                "Location of the Wine prefix directory",
                                row(vec![
                                    text_input("e.g., ~/.wine", &self.wineprefix_input)
                                        .on_input(Message::WineprefixChanged)
                                        .padding(10)
                                        .width(Length::Fill),
                                    button(text("Browse").size(13))
                                        .padding([10, 16])
                                        .on_press(Message::BrowseWineprefix),
                                ])
                                .spacing(8)
                                .width(Length::Fill)
                                .into(),
                            ),
                            self.setting_row(
                                "Architecture",
                                "Wine architecture (win32 or win64)",
                                pick_list(
                                    WineArch::all(),
                                    self.winearch_selection,
                                    Message::WinearchChanged,
                                )
                                .padding(10)
                                .width(Length::Fill)
                                .into(),
                            ),
                            {
                                let current_renderer = self.config.renderer.as_ref()
                                    .map(|r| r.as_str())
                                    .unwrap_or("Auto (default)");
                                let description = format!("Wine Direct3D renderer (renderer=opengl|vulkan) - Current: {}", current_renderer);
                                self.setting_row(
                                    "D3D Renderer",
                                    description.as_str(),
                                    pick_list(
                                        Renderer::all(),
                                        self.renderer_selection,
                                        Message::RendererChanged,
                                    )
                                    .padding(10)
                                    .width(Length::Fill)
                                    .into(),
                                )
                            },
                            {
                                let current_wayland_str = if let Some(ref w) = self.config.wayland {
                                    w.as_str()
                                } else {
                                    match self.config.detect_display_server().as_deref() {
                                        Some("wayland") => "wayland",
                                        Some("xwayland") => "xwayland",
                                        _ => "Auto (detect)",
                                    }
                                };
                                let description = format!("Wine display driver (wayland=wayland|xwayland|auto) - Current: {}", current_wayland_str);
                                self.setting_row(
                                    "Display Driver",
                                    description.as_str(),
                                    pick_list(
                                        WaylandDisplay::all(),
                                        self.wayland_selection,
                                        Message::WaylandChanged,
                                    )
                                    .padding(10)
                                    .width(Length::Fill)
                                    .into(),
                                )
                            },
                        ])
                        .into(),
                    ),
                    self.settings_section(
                        "Installation Options",
                        "Configure how verbs are installed",
                        column(vec![
                            self.checkbox_row(
                                "Force Reinstall",
                                "Don't check if packages are already installed (-f, --force)",
                                self.config.force,
                                Message::ForceToggled,
                            ),
                            self.checkbox_row(
                                "Unattended Mode",
                                "Don't ask questions, install automatically (-q, --unattended)",
                                self.config.unattended,
                                Message::UnattendedToggled,
                            ),
                            self.checkbox_row(
                                "Isolate Prefixes",
                                "Install each app in its own WINEPREFIX (--isolate)",
                                self.config.isolate,
                                Message::IsolateToggled,
                            ),
                            self.checkbox_row(
                                "Keep Temp Directories",
                                "Don't delete temp directories (--no-clean, useful for debugging)",
                                self.config.no_clean,
                                Message::NoCleanToggled,
                            ),
                        ])
                        .into(),
                    ),
                    self.settings_section(
                        "Network Options",
                        "Configure download and network settings",
                        column(vec![
                            self.checkbox_row(
                                "Use Torify",
                                "Run downloads under torify, if available (-t, --torify)",
                                self.config.torify,
                                Message::TorifyToggled,
                            ),
                            self.setting_row(
                                "Country Code",
                                "Set country code to CC (--country=CC)",
                                text_input("e.g., US, GB, DE", &self.country_input)
                                    .on_input(Message::CountryChanged)
                                    .padding(10)
                                    .width(Length::Fixed(100.0))
                                    .into(),
                            ),
                        ])
                        .into(),
                    ),
                    self.settings_section(
                        "Verbosity",
                        "Configure output verbosity levels",
                        column(vec![
                            self.checkbox_row(
                                "Verbose",
                                "Echo all commands as they are executed (-v, --verbose)",
                                self.config.verbosity >= 1,
                                |v| Message::VerbosityChanged(if v { 1 } else { 0 }),
                            ),
                            self.checkbox_row(
                                "Really Verbose",
                                "Really verbose mode (-vv, --really-verbose)",
                                self.config.verbosity >= 2,
                                |v| Message::VerbosityChanged(if v { 2 } else { 0 }),
                            ),
                        ])
                        .into(),
                    ),
                    self.settings_section(
                        "Information",
                        "Paths and directories used by Winetricks",
                        column(vec![
                            self.info_row(
                                "Cache Directory",
                                &self.config.cache_dir.to_string_lossy(),
                            ),
                            self.info_row(
                                "Data Directory",
                                &self.config.data_dir.to_string_lossy(),
                            ),
                            self.info_row(
                                "Prefixes Root",
                                &self.config.prefixes_root.to_string_lossy(),
                            ),
                        ])
                        .into(),
                    ),
                ])
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
                column(vec![
                    text(title).size(20),
                    text(description).size(13),
                    container(content).padding(16),
                ])
                .spacing(12)
                .padding(20),
            )
            .into()
        }

        fn setting_row<'a>(
            &self,
            title: &str,
            description: &str,
            control: Element<'a, Message>,
        ) -> Element<'a, Message> {
            row(vec![
                column(vec![text(title).size(15), text(description).size(12)])
                    .spacing(4)
                    .width(Length::Fill),
                container(control).width(Length::Fixed(300.0)),
            ])
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
            row(vec![
                column(vec![text(title).size(15), text(description).size(12)])
                    .spacing(4)
                    .width(Length::Fill),
                checkbox("", checked).text_size(14).on_toggle(msg_fn),
            ])
            .spacing(16)
            .align_items(Alignment::Center)
            .padding([12, 0])
            .into()
        }

        fn info_row<'a>(&self, title: &str, value: &str) -> Element<'a, Message> {
            row(vec![
                text(title).size(14).width(Length::Fill),
                text(value).size(13),
            ])
            .spacing(16)
            .padding([8, 0])
            .into()
        }

        fn wine_tools_view(&self) -> Element<'_, Message> {
            scrollable(
                column(vec![
                    text("Wine Tools").size(32),
                    text("Quick access to Wine utilities").size(14),
                    self.tool_card(
                        "Wine Configuration",
                        "Configure Wine settings, libraries, and applications",
                        "winecfg",
                    ),
                    self.tool_card(
                        "Registry Editor",
                        "Edit Windows registry entries",
                        "regedit",
                    ),
                    self.tool_card(
                        "Task Manager",
                        "View running processes and applications",
                        "taskmgr",
                    ),
                    self.tool_card(
                        "File Explorer",
                        "Browse Wine filesystem in Windows explorer",
                        "explorer",
                    ),
                    self.tool_card("Uninstaller", "Manage installed programs", "uninstaller"),
                    self.tool_card(
                        "Wine Shell",
                        "Open interactive shell with Wine environment",
                        "shell",
                    ),
                    self.tool_card(
                        "Command Prompt",
                        "Open Windows command prompt (cmd.exe)",
                        "winecmd",
                    ),
                    self.tool_card(
                        "Open Prefix Folder",
                        "Open WINEPREFIX directory in file manager",
                        "folder",
                    ),
                    self.tool_card(
                        "Winetricks Wiki",
                        "Open Winetricks documentation in browser",
                        "help",
                    ),
                ])
                .spacing(12),
            )
            .into()
        }

        fn tool_card<'a>(
            &self,
            title: &str,
            description: &str,
            tool: &str,
        ) -> Element<'a, Message> {
            container(
                button(
                    column(vec![text(title).size(16), text(description).size(13)])
                        .spacing(4)
                        .align_items(Alignment::Start),
                )
                .width(Length::Fill)
                .padding(20)
                .on_press(Message::RunWineTool(tool.to_string())),
            )
            .into()
        }
    }

    fn run_wine_tool(config: &Config, tool: &str) {
        use which::which;
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
                let _ = std::process::Command::new(&wine.wine_bin)
                    .arg("winecfg")
                    .env("WINEPREFIX", &wineprefix)
                    .spawn();
            }
            "regedit" => {
                let mut cmd = std::process::Command::new(&wine.wine_bin);
                cmd.arg("regedit").env("WINEPREFIX", &wineprefix);
                if config.unattended {
                    cmd.arg("/S");
                }
                let _ = cmd.spawn();
            }
            "taskmgr" => {
                let _ = std::process::Command::new(&wine.wine_bin)
                    .arg("taskmgr")
                    .env("WINEPREFIX", &wineprefix)
                    .spawn();
            }
            "explorer" => {
                let _ = std::process::Command::new(&wine.wine_bin)
                    .arg("explorer")
                    .env("WINEPREFIX", &wineprefix)
                    .spawn();
            }
            "uninstaller" => {
                let _ = std::process::Command::new(&wine.wine_bin)
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
                let term = terminals.iter().find(|t| which(*t).is_ok());

                if let Some(term) = term {
                    let term_args = match *term {
                        "gnome-terminal" => vec!["--".to_string(), shell.clone()],
                        _ => vec!["-e".to_string(), shell.clone()],
                    };
                    let _ = std::process::Command::new(*term)
                        .args(term_args)
                        .env("WINEPREFIX", &prefix_path)
                        .env("WINE", &wine_path)
                        .spawn();
                } else {
                    let mut cmd = std::process::Command::new(&shell);
                    cmd.env("WINEPREFIX", &wineprefix)
                        .env("WINE", &wine.wine_bin)
                        .env("WINEDEBUG", "-all");
                    let _ = cmd.spawn();
                }
            }
            "winecmd" => {
                let _ = std::process::Command::new(&wine.wine_bin)
                    .arg("cmd")
                    .env("WINEPREFIX", &wineprefix)
                    .spawn();
            }
            "folder" => {
                let file_managers = ["xdg-open", "open", "cygstart"];
                for fm in &file_managers {
                    if std::process::Command::new(fm)
                        .arg(&wineprefix)
                        .spawn()
                        .is_ok()
                    {
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
                    if std::process::Command::new(browser).arg(url).spawn().is_ok() {
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

    pub fn run_cosmic() -> cosmic::Result {
        WinetricksCosmicApp::run(Settings::default())
    }
}

#[cfg(not(feature = "cosmic"))]
pub mod cosmic_impl {
    pub fn run_cosmic() -> Result<(), String> {
        Err("Cosmic feature is not enabled. Build with --features cosmic".to_string())
    }
}
