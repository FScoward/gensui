/// UIモジュール - TUIの表示とレンダリング機能を提供
///
/// このモジュールはGensui TUIアプリケーションのUI層を構成する。
/// UIロジックをmain.rsから分離し、テストしやすく保守しやすい構造を提供する。

pub mod helpers;
pub mod log_view;
pub mod modals;
pub mod render;
pub mod types;

// Re-export commonly used types and functions
pub use helpers::{centered_rect, format_action_log, permission_mode_label};
pub use log_view::{prepare_raw_log_data, render_detail_tab, render_log_modal, render_overview_tab};
pub use modals::{
    describe_allowed_tools, render_create_selection_modal, render_modal,
    render_name_input_modal, render_permission_modal, render_prompt_modal,
    render_rename_worker_modal, render_tool_selection_modal,
    render_worktree_selection_modal,
};
pub use render::{help_lines, render_footer, render_header, render_table};
pub use types::{LogEntry, LogViewMode, AVAILABLE_TOOLS};
