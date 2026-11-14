//! Checks of the source itself, run as tests: the layout AGENTS.md describes under "Code
//! layout", so a file cannot drift from it unnoticed. They read each file as code only
//! (`code_lines`: comments and the insides of literals blanked), so a string or a comment is
//! never taken for an item, an import or a function. Only `retired_names_stay_gone` also reads
//! the comments, for the names of code in backticks there (`comment_lines`).

use std::collections::HashSet;
use std::path::Path;

/// The names that 0.74.0's naming review retired, because a reader new to the code could not tell
/// from them what they hold or do, and the names of plugins and decoder plugins, which 0.75.0
/// renamed tools and added formats. Beside each is the name that took its place, which is the one
/// to use. `retired_names_stay_gone` fails when one comes back in the code, or in backticks in a
/// comment, which always names code. These names are retired in every file: names of more than one
/// word, abbreviations and private images. An everyday word is in `RETIRED_IN_FILE` instead. A name
/// that is now another thing's right name (`status`, `shown`, `turn`) is not here, nor are the old
/// names of settings and actions, which the `RENAMED_*` tables still read from users' files.
const RETIRED_NAMES: &[&str] = &[
    // The window's parts (main.rs).
    "prefs",         // preferences
    "Prefs",         // Preferences
    "dups",          // find_duplicates
    "Dups",          // FindDuplicates
    "pan_window",    // pan_view
    "print_window",  // print_dialog
    "focus_by_keys", // is_focus_from_keys
    "last_view",     // last_image_area
    "image_id",      // image_area_id
    "notices_tx",    // notice_sender
    "notices_rx",    // notices
    "lightroom_rx",  // lightroom_catalog_answer
    "save_as_last",  // save_as_choices
    "own_program",   // new_window_program
    // The image area (viewer/view.rs).
    "full_ok",       // full_resolution_count
    "set_rotate",    // set_auto_rotate
    "one_at_a_time", // changes_one_pane_at_a_time
    "paint_other",   // paint_panes
    "compare_other", // compared_image
    "files_gen",     // version, list_version
    // The slideshow, Save As and pan view (viewer/).
    "at_start",      // start_after_listing
    "whole_shuffle", // shuffle_next_listing_whole
    "days_stop",     // stop_reading_days
    "add_gone",      // add_removed
    "as_read",       // read_days
    "ymd",           // parse_date
    // The side panel and the status bar (ui/file_list.rs).
    "CELL_EXTRA",         // CELL_PADDING
    "CELL_EXTRA_NO_NAME", // CELL_PADDING_NO_NAME
    "grid_cols",          // grid_columns
    "area_grid_shown",    // is_area_grid_shown
    "area_grid_on",       // is_area_grid_on
    "pane_info",          // pane_images
    "selected_rx",        // selection_sum
    "selection_is",       // set_selection_size
    "scroll_to_cur",      // scroll_to_shown
    "scroll_to_current",  // scroll_to_shown
    "cur_size",           // shown_size
    "forget_place",       // forget_position_text
    // The info panel (ui/info_panel.rs).
    "exif_jobs",          // exif_requests
    "exif_rx",            // exif_answers
    "exif_window",        // is_exif_window_open
    "want_exif",          // request_and_take_exif
    "meta_of",            // metadata_edit_of
    "save_meta",          // save_metadata_edit
    "tag_files",          // set_keyword_on_files
    "exif_panel_setting", // info_panel_setting
    // The look and the window (ui/theme.rs, ui/window.rs).
    "look_rx",       // look_answer
    "screen_rx",     // screen_profile_answer
    "ask_screen",    // ask_screen_profile
    "Ender",         // InhibitorStop
    "inhibit_on",    // inhibit_asked
    "kiosk_quit",    // kiosk_may_quit
    "closes_asked",  // close_requests
    "close_saved",   // turn_tried_at_close
    "turn_saved",    // forget_turn_tried_at_close
    "menu_away",     // pointer_left_menu_at
    "POINTER_STILL", // HIDE_POINTER_AFTER
    "window_shown",  // is_window_visible
    // The dialogs, menus, actions and Preferences (ui/).
    "asked_for",       // asked
    "prompt_answered", // take_prompt_answer
    "answer_prompt",   // act_on_prompt_answer
    "delete_shown",    // delete_dialog_opened_at
    "file_dialog_rx",  // picked_path
    "confirm_folder",  // is_confirming_folder_trash
    "ENTER_AFTER",     // ENTER_IGNORED_FOR
    "delete_what",     // delete_dialog_subject
    "use_screen",      // choose_screen_profile
    "list_act",        // chosen
    "iconed",          // with_icon
    "OWN_KEYS",        // USER_KEYS
    "use_own_keys",    // use_user_keys
    "in_build",        // is_in_build
    "focus_search",    // search_takes_focus
    // What is open: the list, the folder, the search and collections (model/).
    "cur",               // shown
    "set_cur",           // set_shown
    "cur_row",           // shown_row
    "anchor_or_cur",     // anchor_or_shown
    "take_slots",        // take_loaded_images
    "keep_slot",         // keep_loaded_image
    "keep_slots",        // keep_loaded_images
    "forget_slot",       // forget_loaded_image
    "remember_details",  // remember_size_and_date
    "forget_details",    // forget_sizes_and_dates
    "forget_details_of", // forget_size_and_date_of
    "pan_thumb",         // note_pan_thumb
    "right_click",       // select_on_right_click
    "set_unchecked",     // set_unchecked_entries
    "selection_size",    // sum_selection_size
    "quiet_targets",     // targets_without_notice
    "place_of",          // position_text_of
    "only_held_back",    // only_hidden_selected
    "selected_and_held", // selected_with_hidden
    "list_parts",        // lists_tree_in_parts
    "relist",            // relist_when_done, read_folder_again
    "went_to",           // first_part_shown
    "take_files",        // take_listed_files
    "keep_found",        // keep_files_with_size
    "match_rx",          // match_size_rx
    "exif_stale",        // exif_to_read_again
    "holds_exif_reads",  // exif_search_waits_for_writes
    "exif_follow_list",  // index_new_names
    "edit_told",         // edit_open_collection
    // How files change on disk (files/).
    "deleted_sender",  // report_sender
    "deleted_tx",      // report_tx
    "receive_deleted", // receive_operation_report
    "take_renamed",    // take_rename_outcome
    "let_go_of_ended", // drop_ended_jobs
    "none_saving",     // are_free_to_change
    "work_left",       // is_writing
    "same_id",         // same_fingerprint
    "writes_keywords", // is_writing_sidecars
    "writes_of",       // jobs_writing_sidecar_of
    "paths_moved",     // follow_moved_paths
    "fs_type",         // file_system_type_or_test
    "fat_type",        // fat_file_system_type
    "mount_type",      // file_system_type
    "mount_type_in",   // file_system_type_in
    "fs_type_in_udev", // file_system_type_in_udev
    "new_beside",      // create_temp_beside
    "own_tag",         // instance_tag
    "own_dir",         // open_top_trash
    "trash_name",      // name_in_trash
    "Dog",             // WatchdogState
    "dog",             // watchdog
    "host_has",        // host_has_program
    "DEFAULT_DELAY",   // DEFAULT_SLIDESHOW_DELAY
    "MAX_DELAY",       // MAX_SLIDESHOW_DELAY
    // From a file to pixels and back (images/).
    "Hist",                    // HistogramCounts
    "jpeg_ends",               // is_whole_jpeg_file
    "whole_jpeg",              // is_whole_jpeg
    "quarter_turn",            // is_quarter_turn
    "read_opened",             // read_open_file
    "MAX_DECODE",              // MAX_DECODE_BYTES
    "MAX_ANIMATION",           // MAX_ANIMATION_BYTES
    "stem_and",                // stem_with_suffix
    "file_key",                // file_id
    "meta_key",                // file_id_from_metadata
    "key_of",                  // file_id_from_inode
    "replace_holding_sidecar", // replace_with_sidecar_locked
    "like_srgb",               // is_like_srgb
    "worth_marking",           // should_mark_failed
    // What is said about an image (metadata/).
    "parse_access", // parse_alt_text
    "write_access", // write_alt_text
    "set_access",   // set_alt_text
    "held_in",      // lock_sidecar_in
    "held_all",     // lock_sidecars
    "held_all_in",  // lock_sidecars_in
    "update_held",  // update_locked_sidecar
    "lock_slot",    // sidecar_lock_number
    "adobe_rgb",    // is_adobe_rgb
    "put_folder",   // follow_folder
    "put_known",    // follow_known
    // The features with a window of their own (features/).
    "in_block",            // block_matches
    "Fed",                 // WatchdogWriter
    "PAGE_TEXT",           // PAGE_TEXT_HEIGHT
    "layout_of",           // layout_from_settings
    "own_or_none",         // is_own_or_missing
    "dir_of_another",      // is_another_users_dir
    "remote_one",          // handle_remote
    "waiting_select",      // waiting_selects
    "answer_held_selects", // answer_waiting_selects
    // What the rest is built on (support/).
    "CPU_USE",        // CPU_USE_LEVEL
    "ONE_ARENA",      // HAS_ONE_MALLOC_ARENA
    "number_marks",   // number_separators
    "marks_of",       // number_separators_of
    "qualifiers_off", // without_qualifiers
    // Found missing in 0.74.0's first audit.
    "clean_in",          // remove_orphaned_thumbnails_in
    "deleted_rx",        // report_rx
    "jobs_shown",        // first_job_and_count
    "list_place",        // opened_folder_hash
    "other_lines",       // non_entry_lines
    "root_now",          // typed_root_folder
    "take_lightroom",    // takes_lightroom_stars
    "undo_left",         // is_undo_remainder
    "move_held",         // move_lock
    "has_access",        // has_alt_text
    "is_unlocked",       // open_if_unlocked
    "background_paths",  // held_back_files
    "renamed_in_place",  // note_names_changed
    "places_changed",    // forget_places
    "keep_full_only",    // keep_full_resolution_only
    "forget_decoded",    // forget_loaded_images
    "request_exif",      // request_and_take_exif
    "lightroom_catalog", // lightroom_catalog_answer
    // 0.75.0: plugins became tools, and decoder plugins added formats.
    "plugin",                        // tool
    "plugins",                       // tools
    "Plugin",                        // Tool
    "PLUGIN",                        // TOOL
    "RunningPlugins",                // RunningTools
    "plugins_menu",                  // tools_menu
    "plugins_running",               // tools_running
    "reads_and_runs_a_plugin",       // reads_and_runs_a_tool
    "decoder_plugins",               // added_formats
    "installed_decoders",            // installed_formats
    "survives_broken_decoder_files", // survives_broken_format_files
];

/// The everyday words that 0.74.0's naming review retired, each in every file where it was a name
/// in code at v0.73.0 and is none now: the file that defined it and the files that used it. The
/// list was made from v0.73.0's sources with `code_lines`, and the comment gives the name that
/// took the word's place in the file that defined it. Elsewhere such a word may be the right name
/// of something else, as `ratio` of an aspect ratio.
const RETIRED_IN_FILE: &[(&str, &str)] = &[
    ("cli.rs", "more"),                     // more_paths
    ("features/check.rs", "Check"),         // ImageCheck
    ("features/check.rs", "go"),            // show_file
    ("features/duplicates.rs", "SIDE"),     // SIGNATURE_SIDE
    ("features/duplicates.rs", "cancel"),   // is_cancelled
    ("features/duplicates.rs", "caught"),   // catch_panic
    ("features/duplicates.rs", "go"),       // show_file
    ("features/duplicates.rs", "held"),     // lock, lock_sidecar
    ("features/duplicates.rs", "parallel"), // on_all_cores
    ("features/duplicates.rs", "parts"),    // part_means
    ("features/duplicates.rs", "stopped"),  // is_stopped, tell_background_stopped
    ("features/print.rs", "CAPTION"),       // CAPTION_HEIGHT
    ("features/print.rs", "LIMIT"),         // PROGRAM_TIME_LIMIT
    ("features/print.rs", "MM"),            // POINTS_PER_MM
    ("features/print.rs", "PREVIEWED"),     // MAX_PREVIEWS
    ("features/print.rs", "limited"),       // short_output_of
    ("features/print.rs", "place"),         // position_text
    ("features/print.rs", "survive"),       // catch_panic_as_error
    ("features/remote.rs", "ANSWERING"),    // UNFINISHED_ANSWERS
    ("features/remote.rs", "Answering"),    // UnfinishedAnswer
    ("features/remote.rs", "ME"),           // INSTANCE_ID
    ("features/remote.rs", "UNANSWERED"),   // TIMED_OUT
    ("features/remote.rs", "claim"),        // bind_socket
    ("features/remote.rs", "deliver"),      // send_and_print_answer
    ("features/remote.rs", "go"),           // show_file
    ("features/remote.rs", "held"),         // lock, lock_sidecar
    ("features/remote.rs", "known"),        // is_known_command
    ("features/remote.rs", "live"),         // live_sockets
    ("features/remote.rs", "queued"),       // has_waiting_client
    ("features/remote.rs", "unanswered"),   // timed_out
    ("features/tools.rs", "Running"),       // RunningTools
    ("features/tools.rs", "takes"),         // handles_name
    ("files/backup.rs", "FileKey"),         // FileId
    ("files/config.rs", "LOCKED"),          // SETTINGS_LOCKED
    ("files/config.rs", "SAVED"),           // LAST_SAVED_TEXT
    ("files/config.rs", "pos"),             // position
    ("files/external.rs", "LIMIT"),         // PROGRAM_TIME_LIMIT
    ("files/external.rs", "failure"),       // failure_text
    ("files/external.rs", "heif"),          // decode_heif
    ("files/external.rs", "knows"),         // is_program_found
    ("files/external.rs", "limited"),       // short_output_of
    ("files/external.rs", "upright"),       // upright_size
    ("files/file_io.rs", "Work"),           // FileOperations, QueuedWrite
    ("files/file_io.rs", "caught"),         // catch_panic
    ("files/file_io.rs", "land"),           // save_copy_with_sidecar
    ("files/file_io.rs", "plain"),          // explain_rename_error
    ("files/file_io.rs", "serial"),         // start_ordered_worker
    ("files/file_io.rs", "writable"),       // check_writable
    ("files/file_ops.rs", "Deleted"),       // OperationReport
    ("files/file_ops.rs", "Hold"),          // SlideStep::Wait, HeldBackFiles
    ("files/file_ops.rs", "Renamed"),       // RenameOutcome
    ("files/file_ops.rs", "Thrown"),        // TrashMove
    ("files/file_ops.rs", "Work"),          // FileOperations, QueuedWrite
    ("files/file_ops.rs", "begin"),         // start_journal
    ("files/file_ops.rs", "caught"),        // catch_panic
    ("files/file_ops.rs", "details"),       // sizes_and_dates
    ("files/file_ops.rs", "did"),           // count_done
    ("files/file_ops.rs", "doing"),         // activity
    ("files/file_ops.rs", "hold"),          // hold_back_while_writing
    ("files/file_ops.rs", "ids"),           // fingerprints
    ("files/file_ops.rs", "interrupted"),   // cut_short_plans
    ("files/file_ops.rs", "land"),          // save_copy_with_sidecar
    ("files/file_ops.rs", "linked"),        // is_linked
    ("files/file_ops.rs", "listings"),      // whole_listings
    ("files/file_ops.rs", "orient"),        // turn
    ("files/file_ops.rs", "place"),         // position_text
    ("files/file_ops.rs", "refill"),        // refill_preload
    ("files/file_ops.rs", "serial"),        // start_ordered_worker
    ("files/file_ops.rs", "settle"),        // repair_cut_short, run_background_until
    ("files/file_ops.rs", "slots"),         // loaded_images
    ("files/file_ops.rs", "survive"),       // catch_panic_as_error
    ("files/file_ops.rs", "taken"),         // name_in_the_way
    ("files/file_ops.rs", "thrown"),        // trash_move
    ("files/file_ops.rs", "touches"),       // held_back_files
    ("files/file_ops.rs", "touching"),      // held_back_files
    ("files/journal.rs", "begin"),          // start_journal
    ("files/journal.rs", "free"),           // open_if_unlocked
    ("files/journal.rs", "interrupted"),    // cut_short_plans
    ("files/journal.rs", "linked"),         // is_linked
    ("files/journal.rs", "settle"),         // repair_cut_short, run_background_until
    ("files/portal.rs", "DEST"),            // PORTAL_BUS_NAME
    ("files/portal.rs", "PATH"),            // PORTAL_OBJECT_PATH
    ("files/portal.rs", "within"),          // with_time_limit
    ("files/trash.rs", "LIMIT"),            // PROGRAM_TIME_LIMIT
    ("files/trash.rs", "Thrown"),           // TrashMove
    ("files/trash.rs", "Trashed"),          // TrashEntry
    ("files/trash.rs", "thrown"),           // trash_move
    ("images/added_formats.rs", "Decoder"), // AddedFormat
    ("images/added_formats.rs", "LIMIT"),   // PROGRAM_TIME_LIMIT
    ("images/added_formats.rs", "decoder"), // format
    ("images/added_formats.rs", "takes"),   // handles_name
    ("images/colour.rs", "limited"),        // short_output_of
    ("images/colour.rs", "set_up"),         // set_up_colour
    ("images/decode.rs", "Small"),          // ShrunkImage
    ("images/decode.rs", "caught"),         // catch_panic
    ("images/decode.rs", "fits"),           // fits_decode_limit
    ("images/decode.rs", "heif"),           // decode_heif
    ("images/decode.rs", "open_file"),      // open_regular_with_metadata
    ("images/decode.rs", "survive"),        // catch_panic_as_error
    ("images/save.rs", "FileKey"),          // FileId
    ("images/save.rs", "Held"),             // SidecarLock
    ("images/save.rs", "Kind"),             // Encoding
    ("images/save.rs", "LIMIT"),            // PROGRAM_TIME_LIMIT
    ("images/save.rs", "held"),             // lock, lock_sidecar
    ("images/save.rs", "land"),             // save_copy_with_sidecar
    ("images/save.rs", "smaller"),          // shrink_to_long_side
    ("images/save.rs", "without"),          // without_thumbnails, without_property
    ("images/save.rs", "writable"),         // check_writable
    ("images/thumbs.rs", "FAILED"),         // FAILED_DIR
    ("images/thumbs.rs", "clean"),          // remove_orphaned_thumbnails
    ("images/thumbs.rs", "survive"),        // catch_panic_as_error
    ("main.rs", "Check"),                   // ImageCheck
    ("main.rs", "Choice"),                  // SaveAsChoices
    ("main.rs", "FromDesktop"),             // DesktopSettings
    ("main.rs", "Info"),                    // InfoPanel
    ("main.rs", "Notices"),                 // NoticeSender
    ("main.rs", "Pan"),                     // PanView
    ("main.rs", "Running"),                 // RunningTools
    ("main.rs", "View"),                    // ImageArea
    ("main.rs", "Work"),                    // FileOperations, QueuedWrite
    ("main.rs", "act"),                     // chosen
    ("main.rs", "desktop"),                 // desktop_settings, read_desktop_look
    ("main.rs", "rotation"),                // auto_rotate
    ("main.rs", "set_up"),                  // set_up_colour
    ("metadata/exif.rs", "caught"),         // catch_panic
    ("metadata/exif.rs", "mul"),            // compose_orientations
    ("metadata/exif.rs", "ratio"),          // rational
    ("metadata/lightroom.rs", "COLUMN"),    // COLUMN_END
    ("metadata/lightroom.rs", "LIMIT"),     // PROGRAM_TIME_LIMIT
    ("metadata/lightroom.rs", "NEEDS"),     // NEEDS_SQLITE3
    ("metadata/lightroom.rs", "ROW"),       // ROW_END
    ("metadata/lightroom.rs", "differ"),    // differing_stars
    ("metadata/lightroom.rs", "members"),   // collection_images
    ("metadata/lightroom.rs", "stopped"),   // is_stopped, tell_background_stopped
    ("metadata/lightroom.rs", "tagged"),    // image_keyword_ids
    ("metadata/marks.rs", "Seen"),          // FileStamp
    ("metadata/marks.rs", "known"),         // is_known_command
    ("metadata/marks.rs", "seen"),          // file_stamp
    ("metadata/marks.rs", "without"),       // without_thumbnails, without_property
    ("metadata/xmp.rs", "Access"),          // AltText
    ("metadata/xmp.rs", "Held"),            // SidecarLock
    ("metadata/xmp.rs", "Meta"),            // KeywordsAndComment
    ("metadata/xmp.rs", "SLOTS"),           // SIDECAR_LOCKS
    ("metadata/xmp.rs", "access"),          // alt_text
    ("metadata/xmp.rs", "held"),            // lock, lock_sidecar
    ("metadata/xmp.rs", "land"),            // save_copy_with_sidecar
    ("metadata/xmp.rs", "leaf"),            // keyword_of_path
    ("metadata/xmp.rs", "linked"),          // is_linked
    ("metadata/xmp.rs", "locks"),           // lock_dir
    ("metadata/xmp.rs", "same"),            // same_keyword
    ("metadata/xmp.rs", "without"),         // without_thumbnails, without_property
    ("model/collections.rs", "LISTS"),      // LIST_EXTS
    ("model/collections.rs", "act"),        // chosen
    ("model/collections.rs", "lexical"),    // lexically_normal
    ("model/collections.rs", "settle"),     // repair_cut_short, run_background_until
    ("model/collections.rs", "valid"),      // valid_collection_name
    ("model/folder.rs", "Slot"),            // LoadedImage
    ("model/folder.rs", "after"),           // replacing
    ("model/folder.rs", "go"),              // show_file
    ("model/folder.rs", "listings"),        // whole_listings
    ("model/folder.rs", "orient"),          // turn
    ("model/folder.rs", "refill"),          // refill_preload
    ("model/folder.rs", "settle"),          // repair_cut_short, run_background_until
    ("model/folder.rs", "slots"),           // loaded_images
    ("model/folder.rs", "stopped"),         // is_stopped, tell_background_stopped
    ("model/folder.rs", "watch"),           // start_watching
    ("model/folder.rs", "watching"),        // watcher_generation
    ("model/list.rs", "Slot"),              // LoadedImage
    ("model/list.rs", "after"),             // replacing
    ("model/list.rs", "anim"),              // frames
    ("model/list.rs", "details"),           // sizes_and_dates
    ("model/list.rs", "hist"),              // histogram
    ("model/list.rs", "shows"),             // shows_file
    ("model/list.rs", "slots"),             // loaded_images
    ("model/list.rs", "transparent"),       // has_transparency
    ("model/listing.rs", "FileKey"),        // FileId
    ("model/search.rs", "caught"),          // catch_panic
    ("model/search.rs", "settle"),          // repair_cut_short, run_background_until
    ("model/search.rs", "stopped"),         // is_stopped, tell_background_stopped
    ("model/selection.rs", "go"),           // show_file
    ("model/selection.rs", "orient"),       // turn
    ("model/selection.rs", "place"),        // position_text
    ("model/selection.rs", "refill"),       // refill_preload
    ("model/selection.rs", "shows"),        // shows_file
    ("support/lang.rs", "CURRENT"),         // CURRENT_TRANSLATION
    ("support/lang.rs", "Table"),           // Translation
    ("support/source_rules.rs", "Place"),   // SlideshowPosition, Section
    ("support/source_rules.rs", "place"),   // position_text
    ("support/test_hooks.rs", "LABELS"),    // THIS_FRAME
    ("support/test_hooks.rs", "LAST"),      // LAST_FRAME
    ("support/test_hooks.rs", "note"),      // note_label
    ("support/test_support.rs", "settle"),  // repair_cut_short, run_background_until
    ("support/threads.rs", "Notices"),      // NoticeSender
    ("support/threads.rs", "caught"),       // catch_panic
    ("ui/accessibility.rs", "access"),      // alt_text
    ("ui/actions.rs", "Pan"),               // PanView
    ("ui/actions.rs", "launch"),            // open_shown_in
    ("ui/actions.rs", "settle"),            // repair_cut_short, run_background_until
    ("ui/actions.rs", "slots"),             // loaded_images
    ("ui/actions.rs", "stopped"),           // is_stopped, tell_background_stopped
    ("ui/actions.rs", "survive"),           // catch_panic_as_error
    ("ui/dialogs.rs", "Deleted"),           // OperationReport
    ("ui/dialogs.rs", "built_in"),          // is_built_in
    ("ui/dialogs.rs", "go"),                // show_file
    ("ui/dialogs.rs", "place"),             // position_text
    ("ui/dialogs.rs", "settle"),            // repair_cut_short, run_background_until
    ("ui/dialogs.rs", "stopped"),           // is_stopped, tell_background_stopped
    ("ui/dialogs.rs", "touches"),           // held_back_files
    ("ui/dialogs.rs", "unchanged"),         // is_unchanged
    ("ui/file_list.rs", "THUMBS"),          // THUMB_SIZES
    ("ui/file_list.rs", "ThumbView"),       // OnScreenFiles
    ("ui/file_list.rs", "anim"),            // frames
    ("ui/file_list.rs", "details"),         // sizes_and_dates
    ("ui/file_list.rs", "go"),              // show_file
    ("ui/file_list.rs", "made"),            // thumbnail_of
    ("ui/file_list.rs", "place"),           // position_text
    ("ui/file_list.rs", "scrolls"),         // scrolls_to_shown_image
    ("ui/file_list.rs", "slots"),           // loaded_images
    ("ui/info_panel.rs", "Access"),         // AltText
    ("ui/info_panel.rs", "Info"),           // InfoPanel
    ("ui/info_panel.rs", "Meta"),           // KeywordsAndComment
    ("ui/info_panel.rs", "MetaEdit"),       // MetadataEdit
    ("ui/info_panel.rs", "access"),         // alt_text
    ("ui/info_panel.rs", "caught"),         // catch_panic
    ("ui/info_panel.rs", "doing"),          // activity
    ("ui/info_panel.rs", "go"),             // show_file
    ("ui/info_panel.rs", "settle"),         // repair_cut_short, run_background_until
    ("ui/info_panel.rs", "slots"),          // loaded_images
    ("ui/info_panel.rs", "stopped"),        // is_stopped, tell_background_stopped
    ("ui/menus.rs", "Deleted"),             // OperationReport
    ("ui/menus.rs", "act"),                 // chosen
    ("ui/menus.rs", "hold"),                // hold_back_while_writing
    ("ui/menus.rs", "launch"),              // open_shown_in
    ("ui/menus.rs", "refill"),              // refill_preload
    ("ui/menus.rs", "takes"),               // handles_name
    ("ui/preferences.rs", "capturing"),     // key_being_chosen_for
    ("ui/preferences.rs", "clean"),         // remove_orphaned_thumbnails
    ("ui/preferences.rs", "unsaved"),       // has_unsaved
    ("ui/theme.rs", "Desktop"),             // DesktopLook
    ("ui/theme.rs", "FromDesktop"),         // DesktopSettings
    ("ui/theme.rs", "built_in"),            // is_built_in
    ("ui/theme.rs", "desktop"),             // desktop_settings, read_desktop_look
    ("ui/theme.rs", "limited"),             // short_output_of
    ("ui/window.rs", "closing"),            // close_requests
    ("ui/window.rs", "orient"),             // turn
    ("ui/window.rs", "pos"),                // position
    ("ui/window.rs", "settle"),             // repair_cut_short, run_background_until
    ("ui/workers.rs", "ThumbView"),         // OnScreenFiles
    ("ui/workers.rs", "desktop"),           // desktop_settings, read_desktop_look
    ("ui/workers.rs", "slots"),             // loaded_images
    ("ui/workers.rs", "stopped"),           // is_stopped, tell_background_stopped
    ("viewer/crop.rs", "Deleted"),          // OperationReport
    ("viewer/crop.rs", "settle"),           // repair_cut_short, run_background_until
    ("viewer/crop.rs", "slots"),            // loaded_images
    ("viewer/crop.rs", "survive"),          // catch_panic_as_error
    ("viewer/pan.rs", "Pan"),               // PanView
    ("viewer/pan.rs", "calendar"),          // shows_calendar
    ("viewer/pan.rs", "caught"),            // catch_panic
    ("viewer/pan.rs", "each"),              // map_files_in_parallel
    ("viewer/pan.rs", "go"),                // show_file
    ("viewer/pan.rs", "jump"),              // scroll_to_day
    ("viewer/pan.rs", "stopped"),           // is_stopped, tell_background_stopped
    ("viewer/pan.rs", "without"),           // without_thumbnails, without_property
    ("viewer/save_as.rs", "Choice"),        // SaveAsChoices
    ("viewer/save_as.rs", "Deleted"),       // OperationReport
    ("viewer/save_as.rs", "orient"),        // turn
    ("viewer/save_as.rs", "slots"),         // loaded_images
    ("viewer/save_as.rs", "survive"),       // catch_panic_as_error
    ("viewer/slideshow.rs", "Hold"),        // SlideStep::Wait, HeldBackFiles
    ("viewer/slideshow.rs", "Place"),       // SlideshowPosition, Section
    ("viewer/slideshow.rs", "Slide"),       // SlideStep
    ("viewer/slideshow.rs", "go"),          // show_file
    ("viewer/slideshow.rs", "refill"),      // refill_preload
    ("viewer/slideshow.rs", "settle"),      // repair_cut_short, run_background_until
    ("viewer/view.rs", "Slot"),             // LoadedImage
    ("viewer/view.rs", "View"),             // ImageArea
    ("viewer/view.rs", "act"),              // chosen
    ("viewer/view.rs", "go"),               // show_file
    ("viewer/view.rs", "launch"),           // open_shown_in
    ("viewer/view.rs", "made"),             // thumbnail_of
    ("viewer/view.rs", "orient"),           // turn
    ("viewer/view.rs", "overlay"),          // paint_full_screen_info
    ("viewer/view.rs", "place"),            // position_text
    ("viewer/view.rs", "refill"),           // refill_preload
    ("viewer/view.rs", "rotation"),         // auto_rotate
    ("viewer/view.rs", "sent"),             // last_request_number
    ("viewer/view.rs", "slots"),            // loaded_images
    ("viewer/view.rs", "survive"),          // catch_panic_as_error
    ("viewer/view.rs", "transparent"),      // has_transparency
    ("viewer/view.rs", "wheel"),            // wheel_notches
];

/// The calls that clippy.toml refuses (`disallowed-methods`), each kept there: the rules of
/// AGENTS.md on touching the disk, starting programs, decoding, locks and drawing a button
/// rest on them, so a line taken out of clippy.toml fails `the_lints_stay_on`.
const REFUSED_CALLS: &[&str] = &[
    "std::fs::read",
    "std::fs::read_to_string",
    "std::io::read_to_string",
    "std::io::Read::read_to_end",
    "std::fs::write",
    "std::fs::File::create",
    "std::fs::File::create_new",
    "std::fs::OpenOptions::open",
    "std::fs::rename",
    "std::fs::copy",
    "std::fs::remove_file",
    "std::fs::remove_dir",
    "std::fs::remove_dir_all",
    "std::fs::hard_link",
    "std::os::unix::fs::symlink",
    "std::fs::set_permissions",
    "std::fs::File::set_modified",
    "std::fs::File::set_times",
    "image::DynamicImage::save",
    "image::DynamicImage::save_with_format",
    "image::ImageBuffer::save",
    "image::ImageBuffer::save_with_format",
    "image::save_buffer",
    "image::save_buffer_with_format",
    "std::process::Command::new",
    "std::process::Command::output",
    "std::process::Command::status",
    "std::fs::File::open",
    "image::ImageReader::open",
    "image::image_dimensions",
    "image::DynamicImage::from_decoder",
    "image::load_from_memory",
    "image::load_from_memory_with_format",
    "jpeg_decoder::Decoder::decode",
    "image::open",
    "image::load",
    "image::ImageReader::decode",
    "image::ImageReader::no_limits",
    "image::Limits::no_limits",
    "image::ImageDecoder::read_image",
    "image::ImageDecoder::read_image_boxed",
    "image::AnimationDecoder::into_frames",
    "tiff::decoder::Decoder::read_image",
    "std::process::Child::wait",
    "std::process::Child::wait_with_output",
    "egui::Ui::button",
    "egui::Ui::small_button",
    "egui::Ui::selectable_label",
    "egui::Ui::selectable_value",
    "egui::Ui::radio",
    "egui::Ui::radio_value",
    "egui::Button::selectable",
    "egui::Button::new",
    "egui::Ui::checkbox",
    "egui::Checkbox::new",
    "egui::RadioButton::new",
    "egui::Ui::toggle_value",
    "egui::Button::image_and_text",
    "egui::Button::opt_image_and_text",
    "egui::Ui::link",
    "egui::Ui::hyperlink",
    "egui::Ui::hyperlink_to",
    "egui::Ui::menu_button",
    "egui::Ui::menu_image_button",
    "egui::Ui::menu_image_text_button",
    "egui::containers::menu::MenuButton::new",
    "egui::Link::new",
    "egui::Hyperlink::new",
    "egui::Hyperlink::from_label_and_url",
    "std::io::Read::read_to_string",
    "png::Reader::next_frame",
    "std::os::unix::fs::MetadataExt::ino",
    "std::os::linux::fs::MetadataExt::st_ino",
    "std::os::unix::fs::DirEntryExt::ino",
    "glimt::files::file_io::file_lock",
    "std::sync::Mutex::lock",
];

/// The standard library's calls that look at or change the disk, and the system's that Glimt
/// makes through libc. Each is found anywhere in a line, also as a method (`path.read_dir()`).
/// Glimt's own functions that make one of them, such as `read_limited` or the writers of
/// file_io, are not listed: `the_window_thread_never_waits_for_the_disk` follows every call to
/// the functions it reaches. Listed too are the other waits the window must not make: for a
/// program (`run_limited`, `output_of`, `short_output_of`), and for a decode (the three ways
/// into one, `from_decoder`, `scaled` and `from_memory`), which take as long without the disk.
const DISK_CALLS: &[&str] = &[
    "run_limited(",
    "output_of(",
    "short_output_of(",
    // A program waited for under a `Watchdog`, which may take minutes (`lp`, `gio mount`).
    "wait_with_output(",
    ".wait()",
    "from_decoder(",
    "scaled(",
    "from_memory(",
    "fs::metadata(",
    ".metadata()",
    "symlink_metadata(",
    "canonicalize(",
    "read_dir(",
    "read_to_string(",
    "fs::read(",
    "File::open(",
    "File::create(",
    "OpenOptions",
    "DirBuilder",
    ".exists()",
    "try_exists(",
    ".is_file()",
    ".is_dir()",
    ".is_symlink()",
    "read_link(",
    "fs::write(",
    "fs::rename(",
    "fs::copy(",
    "remove_file(",
    "remove_dir",
    "create_dir",
    "set_permissions(",
    "hard_link(",
    "symlink(",
    "xattr(",
    "libc::access(",
    "UnixListener::bind(",
    "UnixStream::connect(",
];

/// The calls that read text from outside Glimt's code: a file read whole, a program's output,
/// and a line from the socket. `outside_text_is_read_by_parsers_fed_broken_input` holds each
/// file that makes one to a `survives_broken_*` test of its parser.
const OUTSIDE_TEXT_READS: &[&str] = &[
    "read_text(",
    "read_limited(",
    "read_unlinked(",
    "output_of(",
    "short_output_of(",
    "read_until(",
];

/// The calls that hand work to another thread, whose text `DISK_CALLS` are not looked for in.
const HANDED_TO_A_THREAD: &[&str] = &[
    "spawn(",
    "apps_for_type(",
    "write_later(",
    "write_later_in_user_folders(",
    "run_in_background(",
    "write_sidecars(",
    "write_in_sidecar_thread(",
    "spawn_worker_pool(",
];

/// What starts work that writes, which closing waits for: a job, work in the background, a
/// sidecar write of the user's keywords, alt text and stars, an operation that will leave an
/// Undo, or the count of writing itself.
const WRITING_WORK: &[&str] = &[
    "start_job(",
    "start_job_of(",
    "run_in_background(",
    "write_sidecars(",
    "undo_on_the_way(",
    "WRITING",
];

/// The ways a text is searched for another in Rust, each as the call written and which of its
/// arguments, counted from 0, is the text searched for: every method of `str` and of slices
/// that looks for a pattern (also `split`, `starts_with`, `strip_prefix`, `replace`, `matches`
/// and their kin), `memchr::memmem`, and the crate's own `ForwardSearch`. A window compared
/// (`windows(n).position(|w| w == name)`) is read apart, in `window_needles`, and a function of
/// the crate that passes its own parameter on to one of these is added to them
/// (`searches_for_xmp_properties`).
const TEXT_SEARCHES: [(&str, usize); 32] = [
    (".contains(", 0),
    (".find(", 0),
    (".rfind(", 0),
    (".match_indices(", 0),
    (".rmatch_indices(", 0),
    (".matches(", 0),
    (".rmatches(", 0),
    (".split_once(", 0),
    (".rsplit_once(", 0),
    (".split(", 0),
    (".rsplit(", 0),
    (".splitn(", 1),
    (".rsplitn(", 1),
    (".split_terminator(", 0),
    (".rsplit_terminator(", 0),
    (".split_inclusive(", 0),
    (".starts_with(", 0),
    (".ends_with(", 0),
    (".strip_prefix(", 0),
    (".strip_suffix(", 0),
    (".trim_start_matches(", 0),
    (".trim_end_matches(", 0),
    (".trim_matches(", 0),
    (".replace(", 0),
    (".replacen(", 0),
    ("memmem::find(", 1),
    ("memmem::rfind(", 1),
    ("memmem::find_iter(", 1),
    ("memmem::rfind_iter(", 1),
    ("Finder::new(", 0),
    ("FinderRev::new(", 0),
    ("ForwardSearch::new(", 1),
];

/// Where a top-level item stands in a file's order, as AGENTS.md gives it under "Within a
/// file"; the variants are in that order.
#[derive(Clone, Copy, PartialEq, PartialOrd)]
enum Section {
    Imports,
    Constants,
    Types,
    AppBlock,
    Functions,
    Tests,
}

impl Section {
    fn name(self) -> &'static str {
        match self {
            Section::Imports => "use and mod",
            Section::Constants => "const and static",
            Section::Types => "types and their impls",
            Section::AppBlock => "the impl crate::App block",
            Section::Functions => "functions",
            Section::Tests => "the tests",
        }
    }
}

/// What a search looks for, as `searched_for` reads it.
#[derive(Clone, Copy, PartialEq)]
enum Searched {
    /// The name of an XMP property, such as "dc:subject" or "<xmp:Rating".
    Property,
    /// The parameter of its function at this place, counted without `self`.
    Parameter(usize),
    /// Anything else, or what the check cannot tell.
    Other,
}

/// A function of the crate outside the tests, as `CallGraph` follows it.
struct Function {
    file: String,
    /// The type of the `impl` or `trait` block it is in, or `None` for a free function.
    owner: Option<String>,
    name: String,
    /// The lines of its file's `code_lines` that it spans, from its `fn` line to its last.
    lines: std::ops::RangeInclusive<usize>,
}

/// A call that a function makes in its own thread, as `CallGraph::calls_of` finds it.
struct Call {
    /// The function of the crate called, or `None` for a call of `DISK_CALLS`.
    callee: Option<usize>,
    /// The call as it is written: `name(`, or `name` for a function given as a value.
    text: String,
    /// Its line in the file, counted from 1.
    line: usize,
}

/// The crate's functions, and how a call in one of them names another. A call is followed by
/// what it says: `self.name(` and `self.field.name(` to the method of that type, `Type::name(`
/// and `module::name(` to that function, `name(` to the free function of that name, and
/// `value.name(` to the method of that name of the type written for `value`, or to every method
/// of that name when no type is written for it (`method_callees`). A name that the function
/// binds itself (a `let`, a parameter, a closure's argument) is its own.
struct CallGraph {
    functions: Vec<Function>,
    /// Each file's `code_lines`.
    lines: std::collections::HashMap<String, Vec<String>>,
    /// The type of each field of the crate's structs, by (struct, field).
    fields: std::collections::HashMap<(String, String), String>,
    /// The free functions by their names.
    free: std::collections::HashMap<String, Vec<usize>>,
    /// The methods and associated functions by their names.
    methods: std::collections::HashMap<String, Vec<usize>>,
}

impl CallGraph {
    fn new(sources: &[(String, String)]) -> Self {
        let mut graph = CallGraph {
            functions: Vec::new(),
            lines: Default::default(),
            fields: field_types(sources),
            free: Default::default(),
            methods: Default::default(),
        };
        for (file, text) in sources {
            let lines = code_lines(text);
            graph.functions.extend(functions_in(file, &lines));
            graph.lines.insert(file.clone(), lines);
        }
        for (i, f) in graph.functions.iter().enumerate() {
            let by_name = match f.owner {
                Some(_) => &mut graph.methods,
                None => &mut graph.free,
            };
            by_name.entry(f.name.clone()).or_default().push(i);
        }
        graph
    }

    /// Returns the calls that function `f` makes in its own thread, line by line, outside the
    /// work it hands to another thread.
    fn calls_of(&self, f: &Function) -> Vec<Call> {
        let lines = &self.lines[&f.file];
        let text = lines[f.lines.clone()].join("\n");
        // Its parameters are in the lines up to the one that opens its body.
        let opens = (f.lines.clone()).find(|&i| lines[i].ends_with('{'));
        let signature = lines[*f.lines.start()..=opens.unwrap_or(*f.lines.start())].join(" ");
        let mut calls = Vec::new();
        let mut handed_over = 0usize;
        for i in f.lines.clone() {
            let code = code_of_this_thread(&lines[i], &mut handed_over);
            let disk = DISK_CALLS.iter().find(|c| code.contains(**c));
            calls.extend(disk.map(|c| Call {
                callee: None,
                text: (*c).to_owned(),
                line: i + 1,
            }));
            // A line that goes on with a method (`.collection()`) gets its receiver from the
            // lines before it (`self` and `.folder`), so that its type can be found.
            let code = match code.trim_start().starts_with('.') {
                true => format!("{}{}", receiver_before(lines, i), code.trim_start()),
                false => code.to_owned(),
            };
            for (callee, text) in self.callees(f, &text, &signature, &code) {
                let line = i + 1;
                let callee = Some(callee);
                calls.push(Call { callee, text, line });
            }
        }
        calls
    }

    /// Returns the functions of the crate that `code`, a part of a line of `f`, calls or gives
    /// as a value, each with the call as it is written. `text` is all of `f`, and `signature`
    /// its lines up to its body.
    fn callees(
        &self,
        f: &Function,
        text: &str,
        signature: &str,
        code: &str,
    ) -> Vec<(usize, String)> {
        let mut found = Vec::new();
        for (at, path) in paths_in(code) {
            let (before, after) = (&code[..at], &code[at + path.len()..]);
            let name = path.rsplit("::").next().unwrap_or(&path);
            let is_call = after.starts_with('(');
            // A function given as a value is told from a local by a name of more than one word
            // or a path, because a local of one word is often named as a function is.
            let is_value = after.starts_with([')', ','])
                && before.trim_end().ends_with(['(', ','])
                && (name.contains('_') || path.contains("::"));
            let is_type = name.starts_with(char::is_uppercase);
            if !(is_call || is_value) || before.ends_with("fn ") || is_type {
                continue;
            }
            let callees = match path.rsplit_once("::") {
                _ if before.ends_with('.') && is_call => {
                    self.method_callees(f, text, signature, before, name)
                }
                _ if before.ends_with('.') => Vec::new(),
                Some((qualifier, _)) => self.qualified_callees(f, qualifier, name),
                None if binds_locally(text, signature, name) => Vec::new(),
                None => self.free_callees(f, name),
            };
            let written = if is_call {
                format!("{name}(")
            } else {
                name.to_owned()
            };
            found.extend(callees.into_iter().map(|c| (c, written.clone())));
        }
        found
    }

    /// Returns the methods `name` that `before` followed by `name(` calls in `f`, whose code is
    /// `text` and whose lines up to its body are `signature`. The receiver's type is found from
    /// `self`, or from the type that `f` writes for a parameter or a `let`, and then through the
    /// types of the fields (`self.a.b`, `other.a`). When it cannot be found, as for a loop's
    /// variable, a closure's argument or what a call returns, every method of that name is
    /// followed, because a rule that guesses must find too much rather than too little.
    fn method_callees(
        &self,
        f: &Function,
        text: &str,
        signature: &str,
        before: &str,
        name: &str,
    ) -> Vec<usize> {
        let receiver = &before[..before.len() - 1];
        // A receiver that ends in a call or an index (`self.folder.current().x(`) starts after
        // it here, so its first word is empty and its type is not found.
        let receiver_start =
            (receiver.rfind(|c: char| !(is_word_char(c) || c == '.'))).map_or(0, |at| at + 1);
        let methods = self.methods.get(name).map_or(&[][..], Vec::as_slice);
        let mut words = receiver[receiver_start..].split('.');
        let mut owner = match words.next() {
            Some("self") => f.owner.clone(),
            Some(word) => match written_type(text, signature, word) {
                Some(written) if written == "Self" => f.owner.clone(),
                written => written,
            },
            None => None,
        };
        for field in words {
            owner = owner.and_then(|o| self.fields.get(&(o, field.to_owned())).cloned());
        }
        // Only a function that takes `self` can be called on a value (`Config::parse(text)` is
        // not what `text.parse()` calls).
        let methods = (methods.iter().copied()).filter(|&m| self.takes_self(m));
        match owner {
            Some(owner) => methods
                .filter(|&m| self.functions[m].owner.as_deref() == Some(owner.as_str()))
                .collect(),
            None => methods.collect(),
        }
    }

    /// Returns whether function `g` takes `self` (`self`, `&self`, `&mut self`, `mut self`).
    fn takes_self(&self, g: usize) -> bool {
        let g = &self.functions[g];
        let lines = &self.lines[&g.file][g.lines.clone()];
        let opens = lines.iter().position(|l| l.ends_with('{')).unwrap_or(0);
        let signature = lines[..=opens].join(" ");
        // The parameters start after `fn name` and its generics, not at the first `(`, which
        // may be the one of `pub(crate)` or of a generic's `Fn(&Path)`.
        let after_name = (signature.match_indices(&format!("fn {}", g.name)))
            .map(|(at, found)| &signature[at + found.len()..])
            .find(|rest| rest.starts_with(['<', '(']))
            .unwrap_or("");
        let mut depth = 0usize;
        let mut previous = ' ';
        let open = after_name.char_indices().find(|&(_, c)| {
            match c {
                '<' => depth += 1,
                '>' if previous != '-' => depth = depth.saturating_sub(1),
                _ => {}
            }
            previous = c;
            c == '(' && depth == 0
        });
        let parameters = open.map_or("", |(at, _)| after_name[at + 1..].trim_start());
        let parameters = parameters.trim_start_matches('&');
        let parameters = parameters.strip_prefix("mut ").unwrap_or(parameters);
        parameters.starts_with("self") && !parameters[4..].starts_with(is_word_char)
    }

    /// Returns the functions that `qualifier::name` names in `f`: a method or associated
    /// function of a type (`Self`, `Folder`), or a free function of a module (`xmp`).
    fn qualified_callees(&self, f: &Function, qualifier: &str, name: &str) -> Vec<usize> {
        let qualifier = qualifier.rsplit("::").next().unwrap_or(qualifier);
        if qualifier.starts_with(char::is_uppercase) {
            let owner = match qualifier {
                "Self" => f.owner.as_deref(),
                _ => Some(qualifier),
            };
            let methods = self.methods.get(name).map_or(&[][..], Vec::as_slice);
            return (methods.iter().copied())
                .filter(|&m| self.functions[m].owner.as_deref() == owner)
                .collect();
        }
        let free = self.free.get(name).map_or(&[][..], Vec::as_slice);
        let module_of = |file: &str| {
            let stem = file.rsplit('/').next().unwrap_or(file);
            stem.trim_end_matches(".rs").to_owned()
        };
        (free.iter().copied())
            .filter(|&g| {
                ["super", "self", "crate"].contains(&qualifier)
                    || module_of(&self.functions[g].file) == qualifier
            })
            .collect()
    }

    /// Returns the free functions `name` that `name(` calls in `f`: the one in `f`'s file when
    /// there is one, as it hides one imported, and else those of every file.
    fn free_callees(&self, f: &Function, name: &str) -> Vec<usize> {
        let free = self.free.get(name).map_or(&[][..], Vec::as_slice);
        let in_file: Vec<usize> = (free.iter().copied())
            .filter(|&g| self.functions[g].file == f.file)
            .collect();
        match in_file.is_empty() {
            true => free.to_vec(),
            false => in_file,
        }
    }
}

/// Every source file and its text: the Rust files by their path below `src/`, also those in
/// folders (`files/config.rs`), and the translations in `po/` as `po/da.po`. A file added is
/// checked without being listed anywhere.
pub(crate) fn sources() -> Vec<(String, String)> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    let mut folders = vec![src.clone()];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                folders.push(path);
                continue;
            }
            let name = path
                .strip_prefix(&src)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            if name.ends_with(".rs") {
                files.push((name, std::fs::read_to_string(&path).unwrap()));
            }
        }
    }
    let po = Path::new(env!("CARGO_MANIFEST_DIR")).join("po");
    for entry in std::fs::read_dir(po).unwrap() {
        let path = entry.unwrap().path();
        let name = format!("po/{}", path.file_name().unwrap().to_str().unwrap());
        if name.ends_with(".po") {
            files.push((name, std::fs::read_to_string(&path).unwrap()));
        }
    }
    files.sort();
    files
}

/// Returns the parts of `App`, each as (file, struct): every struct of the crate that one of
/// `App`'s fields holds, also inside an `Option`. They are read from `struct App`, so a new part
/// is checked without being listed. Their fields stay private (`parts_keep_their_fields`), and
/// their methods run in the window's thread as App's own do
/// (`the_window_thread_never_waits_for_the_disk`).
fn app_parts() -> Vec<(String, String)> {
    parts_of_app(&source("main.rs"), &rust_sources())
}

/// Returns the parts of the `App` in `main`, each as (file, struct), found in `sources`.
fn parts_of_app(main: &str, sources: &[(String, String)]) -> Vec<(String, String)> {
    let main = code_lines(main);
    let start = main.iter().position(|l| l == "struct App {").unwrap();
    let end = block_end(&main, start).unwrap();
    let held: Vec<(Option<String>, &str)> = (main[start + 1..end].iter())
        .filter(|l| indent(l) == 4)
        .filter_map(|l| Some(part_held_by(l.split_once(": ")?.1)))
        .collect();
    let mut parts = Vec::new();
    for (file, text) in sources {
        for line in code_lines(text) {
            let Some(name) = struct_name(&line) else {
                continue;
            };
            let is_held = |(in_file, part): &(Option<String>, &str)| {
                *part == name && in_file.as_ref().is_none_or(|in_file| in_file == file)
            };
            if held.iter().any(is_held) {
                parts.push((file.clone(), name.to_owned()));
            }
        }
    }
    parts
}

/// Returns the type that a field of type `field_type` holds, without the `Option`s around it
/// and without its generic arguments, as (its file when a `crate::` path names it, its name).
fn part_held_by(field_type: &str) -> (Option<String>, &str) {
    let mut held = field_type.trim_end_matches(',');
    while let Some(inner) = (held.strip_prefix("Option<")).and_then(|t| t.strip_suffix('>')) {
        held = inner;
    }
    let without_arguments = held.split('<').next().unwrap_or(held);
    let (module, name) = (without_arguments.rsplit_once("::")).unwrap_or(("", without_arguments));
    let file =
        (module.strip_prefix("crate::")).map(|path| format!("{}.rs", path.replace("::", "/")));
    (file, name)
}

/// Returns the name of the struct with named fields that the top-level `line` starts.
fn struct_name(line: &str) -> Option<&str> {
    let rest = (line.strip_prefix("pub(crate) struct "))
        .or_else(|| line.strip_prefix("pub struct "))
        .or_else(|| line.strip_prefix("struct "))?;
    let name = rest.split(['<', ' ']).next()?;
    rest.ends_with('{').then_some(name)
}

/// The names a `crate::` path can start with: every module, which is a file's name without its
/// folder and `.rs`, and every folder of modules.
fn module_names() -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for (path, _) in rust_sources() {
        let mut parts: Vec<&str> = path.trim_end_matches(".rs").split('/').collect();
        names.push(parts.pop().unwrap().to_owned());
        names.extend(parts.into_iter().map(str::to_owned));
    }
    names.sort();
    names.dedup();
    names
}

/// The names a `crate::` path can start with: main.rs's own modules, a file directly in `src/`
/// or a folder of modules. A module inside a folder is not one, as `crate::config` reaches it
/// only through an import in main.rs.
fn root_names() -> Vec<String> {
    let mut names: Vec<String> = (rust_sources().into_iter())
        .map(|(path, _)| {
            path.trim_end_matches(".rs")
                .split('/')
                .next()
                .unwrap()
                .to_owned()
        })
        .collect();
    names.dedup();
    names
}

/// The Rust files of `sources`.
pub(crate) fn rust_sources() -> Vec<(String, String)> {
    (sources().into_iter())
        .filter(|(name, _)| name.ends_with(".rs"))
        .collect()
}

/// The text of the source file `name`.
fn source(name: &str) -> String {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    std::fs::read_to_string(src.join(name)).unwrap()
}

/// `text`'s lines as code only: comments and the insides of string and char literals blanked,
/// and the blanks at the end of each line gone, so a line closing a block with a comment after
/// it is just `}`. Every code character keeps its line and column.
pub(crate) fn code_lines(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut code = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        match literal_end(&chars, i) {
            Some(end) => {
                let blank = |&c: &char| if c == '\n' { '\n' } else { ' ' };
                code.extend(chars[i..end].iter().map(blank));
                i = end;
            }
            None => {
                code.push(chars[i]);
                i += 1;
            }
        }
    }
    code.lines().map(|l| l.trim_end().to_owned()).collect()
}

/// `text`'s comments only, by line: the code and the literals blanked, so a word in a string is
/// never taken for one in a comment.
fn comment_lines(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut comments = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let end = literal_end(&chars, i).unwrap_or(i + 1);
        let is_comment = chars[i] == '/' && end > i + 1;
        let keep = |&c: &char| if is_comment || c == '\n' { c } else { ' ' };
        comments.extend(chars[i..end].iter().map(keep));
        i = end;
    }
    comments.lines().map(str::to_owned).collect()
}

/// Where the comment or literal that starts at `i` ends (one past its last character); `None`
/// when code starts there.
fn literal_end(c: &[char], i: usize) -> Option<usize> {
    let at = |k: usize| c.get(k).copied();
    let word = |k: usize| at(k).is_some_and(|x| x == '_' || x.is_alphanumeric());
    // One past the first `end` from `from` on, or the end of the text.
    let past = |from: usize, end: &[char]| {
        (from..c.len())
            .find(|&k| c[k..].starts_with(end))
            .map_or(c.len(), |k| k + end.len())
    };
    // An r that starts a raw string (r"…", r#"…"#, br"…") does not end a word.
    let before = i.wrapping_sub(1);
    let raw_r = !word(before) || (at(before) == Some('b') && !word(i.wrapping_sub(2)));
    match (at(i)?, at(i + 1)) {
        ('/', Some('/')) => Some((i..c.len()).find(|&k| c[k] == '\n').unwrap_or(c.len())),
        ('/', Some('*')) => Some(past(i + 2, &['*', '/'])),
        ('"', _) => {
            let mut k = i + 1;
            while k < c.len() && c[k] != '"' {
                k += if c[k] == '\\' { 2 } else { 1 };
            }
            Some((k + 1).min(c.len()))
        }
        // A char literal ('x', '\n', '\'', '\u{…}'), not a lifetime ('a, 'static).
        ('\'', Some('\\')) => Some(past(i + 3, &['\''])),
        ('\'', _) if at(i + 2) == Some('\'') => Some(i + 3),
        ('r', _) if raw_r => {
            let hashes = c[i + 1..].iter().take_while(|&&h| h == '#').count();
            (at(i + 1 + hashes) == Some('"')).then(|| {
                let close: Vec<char> = std::iter::once('"')
                    .chain(std::iter::repeat_n('#', hashes))
                    .collect();
                past(i + 2 + hashes, &close)
            })
        }
        _ => None,
    }
}

/// The string literals of Rust `code` outside its comments: where each starts, and its text
/// with escapes and continued lines read. Byte strings and char literals are left out.
pub(crate) fn literals(code: &str) -> Vec<(usize, String)> {
    let b = code.as_bytes();
    let word = |i: usize| i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
    let (mut out, mut i) = (Vec::new(), 0);
    while i < b.len() {
        let rest = &code[i..];
        let raw_hashes = (b[i] == b'r')
            .then(|| rest[1..].find(|c| c != '#'))
            .flatten()
            .filter(|&n| rest[1 + n..].starts_with('"'));
        if rest.starts_with("//") {
            i += rest.find('\n').unwrap_or(rest.len());
        } else if rest.starts_with("/*") {
            i += rest.find("*/").map_or(rest.len(), |n| n + 2);
        } else if b[i] == b'\'' {
            // A char literal ('"', '\''), or a lifetime ('a), which has no end.
            let end = match rest[1..].chars().next().unwrap_or(' ') {
                '\\' => rest.get(3..).and_then(|r| r.find('\'')).map(|n| n + 3),
                c => Some(1 + c.len_utf8()).filter(|&e| rest[e..].starts_with('\'')),
            };
            i += end.map_or(1, |e| e + 1);
        } else if let Some(hashes) = raw_hashes
            && (!word(i) || (b[i - 1] == b'b' && !word(i - 1)))
        {
            let close = format!("\"{}", "#".repeat(hashes));
            let body = 2 + hashes;
            let len = rest[body..].find(&close).unwrap_or(rest.len() - body);
            if !word(i) {
                out.push((i, rest[body..body + len].to_owned()));
            }
            i += body + len + close.len();
        } else if b[i] == b'"' {
            let (mut text, mut chars) = (String::new(), rest[1..].char_indices().peekable());
            let mut end = rest.len();
            while let Some((k, c)) = chars.next() {
                if c == '"' {
                    end = k + 2;
                    break;
                }
                if c != '\\' {
                    text.push(c);
                    continue;
                }
                match chars.next().map(|e| e.1) {
                    Some('n') => text.push('\n'),
                    Some('t') => text.push('\t'),
                    Some('\n') => while chars.next_if(|e| e.1.is_whitespace()).is_some() {},
                    Some('u') => {
                        let hex: String = (chars.by_ref().map(|e| e.1))
                            .take_while(|&c| c != '}')
                            .filter(char::is_ascii_hexdigit)
                            .collect();
                        text.extend(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32));
                    }
                    Some(c) => text.push(c),
                    None => {}
                }
            }
            if !(i > 0 && b[i - 1] == b'b' && !word(i - 1)) {
                out.push((i, text));
            }
            i += end;
        } else {
            i += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    out
}

/// The literals read whole: escapes, continued lines, raw and byte strings, char literals
/// and lifetimes, comments.
#[test]
fn reads_literals() {
    let code = "tr(\"a \\\"b\\\" \\\n    c\\n\\u{e9}\"); // \"no\"\n/* \"no\" */ b\"no\" '\"' '\\'' \
                &'static str r#\"x \"y\" z\"# br\"no\" '\\u{feff}' \"\"";
    let texts: Vec<String> = literals(code).into_iter().map(|l| l.1).collect();
    assert_eq!(texts, ["a \"b\" c\né", "x \"y\" z", ""]);
}

/// The rules find a function at any indent, and a `Sense` that takes a click in every spelling.
#[test]
fn finds_functions_and_clicks() {
    assert_eq!(
        function_name("    pub(crate) fn turn<T>(x: T)"),
        Some("turn")
    );
    assert_eq!(function_name("        fn inner() {"), Some("inner"));
    assert_eq!(function_name("    let f = 1;"), None);
    for click in [
        "ui.interact(rect, id, egui::Sense::CLICK)",
        "ui.allocate_rect(r, Sense::click())",
        "ui.interact(r, id, egui::Sense::click_and_drag())",
        "ui.interact(r, id, egui::Sense::all())",
        "ui.interact(r, id, Sense::hover() | Sense::CLICK)",
    ] {
        assert!(takes_a_click(click), "{click}");
    }
    for none in [
        "ui.interact(r, id, egui::Sense::hover())",
        "Sense::DRAG",
        "let s: Sense = x;",
    ] {
        assert!(!takes_a_click(none), "{none}");
    }
}

/// `line` without what may stand before `fn`, `struct` and the like: a visibility, `unsafe`,
/// `async`, and `const` or `extern "C"` when a function follows (a const fn is a function, not
/// a constant).
fn without_qualifiers(line: &str) -> &str {
    let mut l = line;
    loop {
        let before = l;
        if let Some((_, rest)) = l.strip_prefix("pub(").and_then(|r| r.split_once(") ")) {
            l = rest;
        }
        for prefix in ["pub ", "unsafe ", "async "] {
            l = l.strip_prefix(prefix).unwrap_or(l);
        }
        for prefix in ["const ", "extern \"C\" "] {
            let function = |r: &&str| without_qualifiers(r).starts_with("fn ");
            if let Some(rest) = l.strip_prefix(prefix).filter(function) {
                l = rest;
            }
        }
        if l == before {
            return l;
        }
    }
}

/// Returns the name of the function that `line` starts, at any indent (a method, or a function
/// inside another), or `None` when it starts none. Every rule that asks which function a line
/// is in finds it here, so none of them misses an indented one.
fn function_name(line: &str) -> Option<&str> {
    let rest = without_qualifiers(line.trim_start()).strip_prefix("fn ")?;
    rest.split(['(', '<']).next()
}

/// The name of the constant or static that starts on `line` at the top level, such as
/// `SHORTCUTS`; `None` for any other line.
fn constant_name(line: &str) -> Option<&str> {
    let l = without_qualifiers(line);
    let rest = (l
        .strip_prefix("const ")
        .or_else(|| l.strip_prefix("static ")))?;
    (!line.starts_with(' ')).then(|| rest.split([':', ' ']).next())?
}

/// Whether `line` names Enter or Esc other than as the key of an `answers(…, Key)` call, or
/// renames `Key` or imports its variants so that a key could be named without it.
fn names_enter_or_esc(line: &str) -> bool {
    // A key renamed by a `use` or a type alias, or named by its text, hides `Key::Escape`.
    let trimmed = without_qualifiers(line.trim_start());
    let is_alias = trimmed.starts_with("type ") && line.ends_with("Key;");
    if ["Key as ", "Key::*", "Key::{", "Key::from_name(\""]
        .iter()
        .any(|t| line.contains(t))
        || is_alias
    {
        return true;
    }
    ["Key::Enter", "Key::Escape"].iter().any(|key| {
        line.match_indices(key).any(|(at, _)| {
            let before = &line[..at];
            let before = before.strip_suffix("egui::").unwrap_or(before);
            // The key is the second and last argument of the nearest `answers(` before it.
            let argument = before
                .rfind("answers(")
                .map(|a| &before[a + "answers(".len()..]);
            let answer = argument
                .is_some_and(|a| a.ends_with(", ") && a.matches(", ").count() == 1)
                && line[at + key.len()..].starts_with(')');
            !answer
        })
    })
}

/// Whether `line` names a `Sense` that takes a click, in any of egui's spellings
/// (`Sense::click()`, `Sense::CLICK`, `Sense::click_and_drag()`, `Sense::all()`, a union). Every
/// spelling counts except the few that take no click, so a new one is caught, not missed.
fn takes_a_click(line: &str) -> bool {
    const NO_CLICK: &[&str] = &[
        "hover",
        "HOVER",
        "drag",
        "DRAG",
        "focusable_noninteractive",
        "FOCUSABLE",
        "empty",
    ];
    line.match_indices("Sense::").any(|(at, sense)| {
        let rest = &line[at + sense.len()..];
        let name = rest
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .next();
        !NO_CLICK.contains(&name.unwrap_or(""))
    })
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// The line of the `}` that closes the block whose first line is `i`: the next line at the same
/// indent that starts with `}`.
fn block_end(lines: &[String], i: usize) -> Option<usize> {
    let closes = |j: &usize| indent(&lines[*j]) == indent(&lines[i]);
    (i + 1..lines.len()).find(|j| closes(j) && lines[*j].trim_start().starts_with('}'))
}

/// The line where the tests module starts in `lines`, or their end without one.
fn tests_start(lines: &[String]) -> usize {
    (lines.iter())
        .position(|l| without_qualifiers(l) == "mod tests {")
        .unwrap_or(lines.len())
}

/// The section of the top-level item that starts on `line` (a line of `code_lines`); `None` for a
/// line that starts none (an attribute, the rest of an item, a blank).
fn section_of(line: &str) -> Option<Section> {
    let l = without_qualifiers(line);
    let starts = |word: &str| {
        l.strip_prefix(word)
            .is_some_and(|rest| rest.starts_with([' ', '<', '!', '(', '"']))
    };
    let for_app = l.contains(" for App ") || l.contains(" for crate::App ");
    let types = ["struct", "enum", "type", "trait", "union", "impl"];
    let macro_call = l.split_once('!').is_some_and(|(name, _)| {
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    });
    Some(if l == "mod tests {" {
        Section::Tests
    } else if starts("use") || starts("mod") || starts("macro_rules") || starts("extern crate") {
        Section::Imports
    } else if starts("const") || starts("static") {
        Section::Constants
    } else if l == "impl App {" || l == "impl crate::App {" || (starts("impl") && for_app) {
        Section::AppBlock
    } else if types.iter().any(|w| starts(w)) {
        Section::Types
    } else if starts("fn") || starts("extern") {
        Section::Functions
    } else if macro_call {
        // A macro that makes items, such as config!.
        Section::Types
    } else {
        return None;
    })
}

/// Each file's items in AGENTS.md's order: use and mod, const and static, types with their
/// impls, one `impl crate::App` block, functions, and the tests last.
#[test]
fn items_are_in_order() {
    let mut wrong = Vec::new();
    for (name, text) in rust_sources() {
        let lines = code_lines(&text);
        let (mut last, mut apps, mut i) = (Section::Imports, 0, 0);
        while i < lines.len() {
            let line = &lines[i];
            let Some(here) = section_of(line) else {
                i += 1;
                continue;
            };
            if line == "impl App {" || line == "impl crate::App {" {
                apps += 1;
                if apps == 2 {
                    wrong.push(format!("{name}:{}: a second impl crate::App block", i + 1));
                }
            }
            if here < last {
                let (what, after) = (here.name(), last.name());
                wrong.push(format!("{name}:{}: {what} after {after}", i + 1));
            }
            last = here;
            // Nothing inside the tests module is ranked; whatever comes after it is.
            i = match here {
                Section::Tests => block_end(&lines, i).unwrap_or(lines.len()) + 1,
                _ => i + 1,
            };
        }
    }
    assert!(wrong.is_empty(), "out of order:\n{}", wrong.join("\n"));
}

/// Every test's own folder or file in the temporary folder has a name no other test uses.
/// Tests run at once, and two that shared `glimt-test-esc-{pid}` removed each other's folder.
/// The names are compared with each `{…}` of a format as `{}`, so `glimt-test-link-{}` and
/// `glimt-test-link-{pid}` are one name. Those two were not, and a symbolic link of the trash's
/// test and a folder of `writes_through_links` were made at one path, so one of them failed
/// with EEXIST when they ran at once.
#[test]
fn tests_use_temporary_names_of_their_own() {
    // The literal with each `{…}` as `{}`.
    let without_format_arguments = |literal: &str| {
        let mut name = String::new();
        let mut rest = literal;
        while let Some((before, after)) = rest.split_once('{') {
            name += before;
            name += "{}";
            rest = after.split_once('}').map_or("", |(_, after)| after);
        }
        name + rest
    };
    let mut seen: std::collections::HashMap<String, String> = Default::default();
    let mut wrong = Vec::new();
    for (name, text) in sources().iter().filter(|(n, _)| n.ends_with(".rs")) {
        for (at, _) in text.match_indices("\"glimt-test-") {
            let rest = &text[at + 1..];
            let Some(end) = rest.find('"') else { continue };
            let literal = without_format_arguments(&rest[..end]);
            if let Some(first) = seen.insert(literal.clone(), name.clone()) {
                wrong.push(format!("{literal}: {first} and {name}"));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "the same temporary name in two tests:\n{}",
        wrong.join("\n")
    );
}

/// A test sits in the tests of the module whose code it tests. Once main.rs's tests module held
/// what such tests start from (`app_on`, `run_background_until`), and that drew every test that
/// needed a whole `App` into main.rs. Those helpers are in test_support.rs now, and only the tests
/// of main.rs's own code stay in main.rs.
#[test]
fn tests_sit_beside_their_code() {
    const OWN: &[&str] = &[
        "an_app_lists_its_folder_without_a_window",
        "tells_settings_problems_but_unknown_ones",
        "new_window_program",
    ];
    let code = code_lines(&source("main.rs"));
    let mut lines = code[tests_start(&code)..].iter().map(|l| l.trim());
    while let Some(line) = lines.next() {
        if line == "#[test]" {
            // Past other attributes, such as #[ignore], to the function.
            let name = (lines.by_ref())
                .find(|l| !l.is_empty() && !l.starts_with("#["))
                .unwrap();
            let name = name.trim_start_matches("fn ").split('(').next().unwrap();
            assert!(
                OWN.contains(&name),
                "{name} is in main.rs: put it in the tests of the module it tests, with \
                 crate::support::test_support::app_on for an App, or list it in OWN if it tests main.rs"
            );
        }
    }
}

/// App holds its parts, each a struct in the module whose code uses it, and not much else:
/// 170 fields side by side once made every module free to touch every other's state. A new
/// part's state goes in a struct in its module; a field more here fails this.
#[test]
fn app_stays_small() {
    const MOST: usize = 42;
    let lines = code_lines(&source("main.rs"));
    let start = lines.iter().position(|l| l == "struct App {").unwrap();
    let end = block_end(&lines, start).unwrap();
    let fields = (lines[start + 1..end].iter())
        .filter(|l| indent(l) == 4 && !l.trim_start().starts_with("#["))
        .count();
    assert!(
        fields <= MOST,
        "App has {fields} fields: put a new part's state in a struct in its module, as \
         slideshow::Slideshow is, rather than in App"
    );
}

/// A part of App keeps its fields to itself: only its module changes them, and other modules
/// call methods whose names say what happens (`self.slideshow.toggle_pause()`), so the
/// compiler, not a convention, keeps a part's rules in one place. A public field in one of the
/// structs of `app_parts` fails this, and so does a part's name given to two
/// structs, which would leave one of them unchecked.
#[test]
fn parts_keep_their_fields() {
    let mut wrong = Vec::new();
    let parts = app_parts();
    for (file, part) in &parts {
        let lines = code_lines(&source(file));
        if (parts.iter()).any(|(other, name)| name == part && other != file) {
            wrong.push(format!("{file}: a struct {part} in another file too"));
        }
        let start = (lines.iter())
            .position(|l| struct_name(l) == Some(part.as_str()))
            .unwrap();
        let end = block_end(&lines, start).unwrap();
        for (i, line) in lines.iter().enumerate().take(end).skip(start + 1) {
            if indent(line) == 4 && line.trim_start().starts_with("pub") {
                wrong.push(format!("{file}:{}: {part}'s field", i + 1));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "make these private, and give the part a method that says what happens:\n{}",
        wrong.join("\n")
    );
}

/// The parts are found from App's fields: a struct held directly, by its path, or in
/// `Option`s, also with generic arguments. A tuple struct, a type of the standard library, a
/// struct inside a macro and one of the same name in another module than the path names are
/// not parts.
#[test]
fn finds_the_parts_of_app() {
    let main = "struct App {\n    /// A field.\n    gallery: crate::viewer::gallery::Gallery,\n    \
        check: Option<Option<Check>>,\n    pages: Pages<u8>,\n    \
        answer: Receiving<(Result<Import, String>, Working)>,\n    \
        #[cfg(feature = \"x\")]\n    busy: Arc<AtomicUsize>,\n}\n";
    let sources = [
        (
            "viewer/gallery.rs".to_owned(),
            "pub(crate) struct Gallery {\n    pub shown: usize,\n}\n".to_owned(),
        ),
        (
            "viewer/other.rs".to_owned(),
            "pub(crate) struct Gallery {\n    pub shown: usize,\n}\n".to_owned(),
        ),
        (
            "a.rs".to_owned(),
            "struct Check {\n}\npub struct Pages<T> {\n}\npub(crate) struct Receiving<T>(u8);\n\
             macro_rules! m {\n    pub struct Import {\n    }\n}\n"
                .to_owned(),
        ),
    ];
    let parts = parts_of_app(main, &sources);
    let parts: Vec<(&str, &str)> = (parts.iter())
        .map(|(file, part)| (file.as_str(), part.as_str()))
        .collect();
    assert_eq!(
        parts,
        [
            ("viewer/gallery.rs", "Gallery"),
            ("a.rs", "Check"),
            ("a.rs", "Pages")
        ]
    );
}

/// The retired names in `text`, the file `file`, each as "file:line: name". It reads the code, and
/// the words in backticks in the comments, as a word in backticks always names code. A comment
/// may still say an old word in plain prose.
fn retired_names_in(file: &str, text: &str) -> Vec<String> {
    let is_retired =
        |word: &str| RETIRED_NAMES.contains(&word) || RETIRED_IN_FILE.contains(&(file, word));
    let word_list = |part: &str| -> Vec<String> {
        (part.split(|c: char| c != '_' && !c.is_alphanumeric()))
            .filter(|word| is_retired(word))
            .map(str::to_owned)
            .collect()
    };
    let mut found = Vec::new();
    for (i, line) in code_lines(text).iter().enumerate() {
        found.extend(
            word_list(line)
                .into_iter()
                .map(|w| format!("{file}:{}: {w}", i + 1)),
        );
    }
    for (i, line) in comment_lines(text).iter().enumerate() {
        let in_backticks = line.split('`').skip(1).step_by(2);
        for word in in_backticks.flat_map(word_list) {
            found.push(format!("{file}:{}: `{word}` in a comment", i + 1));
        }
    }
    found
}

/// A name that 0.74.0's naming review retired stays out of the code and out of the comments'
/// names of code, so the next change does not bring it back by habit or by copying older code.
#[test]
fn retired_names_stay_gone() {
    let mut wrong = Vec::new();
    for (file, text) in rust_sources() {
        wrong.extend(retired_names_in(&file, &text));
    }
    assert!(
        wrong.is_empty(),
        "retired names (RETIRED_NAMES and RETIRED_IN_FILE say what took their place):\n{}",
        wrong.join("\n")
    );
}

/// The guard finds a retired name in code and in backticks in a comment, and an everyday word
/// only in the file where it was retired. Plain prose and texts may say the old word.
#[test]
fn retired_names_are_found_in_code_and_in_backticks() {
    let text = "// Under `claim`'s lock, and cur in prose.\nlet cur = ratio(\"cur\");\n";
    assert_eq!(
        retired_names_in("features/remote.rs", text),
        [
            "features/remote.rs:2: cur",
            "features/remote.rs:1: `claim` in a comment"
        ]
    );
    assert_eq!(
        retired_names_in("metadata/exif.rs", text),
        ["metadata/exif.rs:2: cur", "metadata/exif.rs:2: ratio"]
    );
    // A word is also retired in a file that only used it, as view.rs used list.rs's `slots`.
    assert_eq!(
        retired_names_in("viewer/view.rs", "let slots = 1;\n"),
        ["viewer/view.rs:1: slots"]
    );
    // The names of 0.75.0's tools and added formats: their old folders stay in strings.
    let text = "use crate::images::decoder_plugins;\nlet plugins = [\"plugins\"];\n";
    assert_eq!(
        retired_names_in("ui/menus.rs", text),
        ["ui/menus.rs:1: decoder_plugins", "ui/menus.rs:2: plugins"]
    );
}

/// A function a reader takes in whole: at most 120 lines outside the tests. The exceptions are
/// tables of one arm per action (`table_is_short`), whose arms stay a few lines each: a longer
/// arm is a method of its own, in the module of what it does.
#[test]
fn functions_stay_short() {
    const MOST: usize = 120;
    const TABLES: &[(&str, &str)] = &[
        ("ui/actions.rs", "english"),
        ("ui/actions.rs", "icon"),
        ("ui/actions.rs", "target"),
        ("ui/actions.rs", "run"),
    ];
    let mut wrong = Vec::new();
    for (file, text) in rust_sources() {
        let lines = code_lines(&text);
        let end = tests_start(&lines);
        for i in 0..end {
            let Some(rest) = without_qualifiers(lines[i].trim_start()).strip_prefix("fn ") else {
                continue;
            };
            let name = rest.split(['(', '<']).next().unwrap_or("");
            // A declaration without a body (in a trait, an extern block), or a function on one
            // line: nothing to measure.
            let head_end = (i..end).find(|&j| lines[j].ends_with(['{', ';', '}']));
            if head_end.is_none_or(|j| !lines[j].ends_with('{')) {
                continue;
            }
            let Some(last) = block_end(&lines, i) else {
                continue;
            };
            if !TABLES.contains(&(file.as_str(), name)) {
                if last - i + 1 > MOST {
                    let n = last - i + 1;
                    wrong.push(format!("{file}:{}: {name} has {n} lines", i + 1));
                }
                continue;
            }
            if let Err(e) = table_is_short(&lines, i, last) {
                wrong.push(format!("{file}:{}: {name} {e}", i + 1));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "split these into functions of what each part does:\n{}",
        wrong.join("\n")
    );
}

/// A table, lines `first..=last` of a function: a few lines that set it up, then one `match`
/// that ends it, whose arms (pattern, guard and body, a pattern's `| …` lines aside) are short.
fn table_is_short(lines: &[String], first: usize, last: usize) -> Result<(), String> {
    const SETUP: usize = 20;
    const ARM: usize = 8;
    let solid = |range: std::ops::Range<usize>| range.filter(|&j| !lines[j].is_empty()).count();
    let body = indent(&lines[first]) + 4;
    // The last match at the body's indent, which must end the function.
    let table = (first + 1..last)
        .rev()
        .find(|&j| indent(&lines[j]) == body && lines[j].trim_start().starts_with("match "))
        .filter(|&m| {
            block_end(lines, m).is_some_and(|e| (e + 1..last).all(|j| lines[j].is_empty()))
        })
        .ok_or("does not end in its match")?;
    if solid(first..table) > SETUP {
        return Err("does much before its match".into());
    }
    // An arm starts at the arms' indent with anything but the rest of a pattern or a brace.
    let arm = indent(&lines[table]) + 4;
    let starts: Vec<usize> = (table + 1..last - 1)
        .filter(|&j| {
            indent(&lines[j]) == arm
                && !lines[j].trim_start().starts_with(['|', '{', '}', ')', ']'])
        })
        .chain([last - 1])
        .collect();
    for pair in starts.windows(2) {
        // A pattern's `| …` lines sit at the arms' indent; a `||` or closure in a body is deeper.
        let pattern = (pair[0]..pair[1])
            .filter(|&j| indent(&lines[j]) == arm && lines[j].trim_start().starts_with('|'));
        if solid(pair[0]..pair[1]) - pattern.count() > ARM {
            return Err(format!("has a long arm at line {}", pair[0] + 1));
        }
    }
    Ok(())
}

/// The first name of every `crate::` path in `lines`, with its line number: in `crate::{…}`,
/// also over several lines, the first name of each entry (nested braces belong to their
/// entry); `*` for a glob.
fn crate_paths(lines: &[String]) -> Vec<(usize, String)> {
    let code = lines.join("\n");
    let line_of = |at: usize| code[..at].matches('\n').count() + 1;
    let word = |c: char| c == '_' || c.is_alphanumeric();
    let mut paths = Vec::new();
    for (at, _) in code.match_indices("crate::") {
        // Not the end of another word, nor $crate in a macro.
        if code[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c == '$' || word(c))
        {
            continue;
        }
        let rest = &code[at + "crate::".len()..];
        let Some(inner) = rest.strip_prefix('{') else {
            let name: String = rest.chars().take_while(|&c| word(c) || c == '*').collect();
            paths.push((line_of(at), name));
            continue;
        };
        let (mut depth, mut name, mut taking) = (1, String::new(), true);
        // Where the name being read starts in `code`, for its line.
        let mut entry = at;
        let base = code.len() - inner.len();
        for (from, c) in inner.char_indices().map(|(k, c)| (base + k, c)) {
            match c {
                '{' => (depth, taking) = (depth + 1, false),
                '}' => depth -= 1,
                ',' if depth == 1 => {
                    paths.push((line_of(entry), std::mem::take(&mut name)));
                    taking = true;
                }
                c if depth == 1 && taking && (word(c) || c == '*') => {
                    if name.is_empty() {
                        entry = from;
                    }
                    name.push(c)
                }
                _ if !name.is_empty() => taking = false,
                _ => {}
            }
            if depth == 0 {
                paths.push((line_of(entry), name));
                break;
            }
        }
    }
    paths.retain(|(_, name)| !name.is_empty());
    paths
}

/// A name is imported from the module that defines it, `crate::files::config::Config`, not through
/// main.rs's own imports (`crate::Config`), which hide where it lives and break when main.rs
/// stops using it; and never `crate::*`.
#[test]
fn imports_name_where_defined() {
    let files = rust_sources();
    let modules = root_names();
    let main = code_lines(&source("main.rs"));
    let main_end = tests_start(&main);
    let words = [
        "struct ", "enum ", "fn ", "type ", "const ", "static ", "trait ", "mod ",
    ];
    // main.rs's lines at the top level, and in a top-level extern block (malloc_trim).
    let mut in_extern = false;
    let top: Vec<&str> = (main[..main_end].iter())
        .filter(|l| {
            let opens = without_qualifiers(l).starts_with("extern ") && l.ends_with('{');
            in_extern = (in_extern || opens) && l.as_str() != "}";
            indent(l) == 0 || in_extern
        })
        .map(|l| without_qualifiers(l.trim_start()))
        .collect();
    // An item main.rs defines there, a macro exported to the crate root, or main.rs's tests
    // module. A macro that is not exported, such as config!, is no name at the root. The code
    // without its comments is searched, so an attribute that is commented out does not count.
    let code: Vec<String> = (files.iter())
        .map(|(_, t)| code_lines(t).join("\n"))
        .collect();
    let defined = |name: &str| {
        let defines = |l: &&str| {
            words.iter().any(|w| {
                (l.strip_prefix(w).and_then(|r| r.strip_prefix(name)))
                    .is_some_and(|r| !r.starts_with(|c: char| c == '_' || c.is_alphanumeric()))
            })
        };
        let macro_rule = format!("#[macro_export]\nmacro_rules! {name} ");
        top.iter().any(defines) || code.iter().any(|c| c.contains(&macro_rule)) || name == "tests"
    };
    let mut wrong = Vec::new();
    for (file, text) in &files {
        for (line, name) in crate_paths(&code_lines(text)) {
            if !modules.contains(&name) && !defined(&name) {
                wrong.push(format!("{file}:{line}: crate::{name}"));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "import these from the module that defines them:\n{}",
        wrong.join("\n")
    );
}

/// In a file's own code `super::` is the crate root, main.rs: the same way round its imports
/// as `crate::Name`. It is for the tests and inline modules, where it is the file's module.
#[test]
fn super_only_inside_modules() {
    let mut wrong = Vec::new();
    for (file, text) in rust_sources() {
        let lines = code_lines(&text);
        let mut i = 0;
        while i < lines.len() {
            let line = &lines[i];
            if without_qualifiers(line).starts_with("mod ") && line.ends_with('{') {
                i = block_end(&lines, i).unwrap_or(lines.len()) + 1;
                continue;
            }
            if line.contains("super::") {
                wrong.push(format!("{file}:{}", i + 1));
            }
            i += 1;
        }
    }
    assert!(
        wrong.is_empty(),
        "super:: outside the tests:\n{}",
        wrong.join("\n")
    );
}

/// main.rs imports from its modules as every other file does, `use crate::ui::actions::…`, not
/// `use actions::…`.
#[test]
fn main_names_its_modules_with_crate() {
    let modules = module_names();
    for (i, line) in code_lines(&source("main.rs")).iter().enumerate() {
        let first = (without_qualifiers(line).strip_prefix("use "))
            .and_then(|l| l.split("::").next())
            .filter(|m| modules.iter().any(|x| x == m));
        assert!(
            first.is_none(),
            "main.rs:{}: use crate::{}",
            i + 1,
            first.unwrap_or_default()
        );
    }
}

/// AGENTS.md's table under "Code layout" has a row for every source file, and no row for a
/// file that is gone: a module added or renamed is described where a reader looks first.
#[test]
fn agents_md_lists_every_module() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let agents = std::fs::read_to_string(root.join("AGENTS.md")).unwrap();
    let rows: Vec<&str> = (agents.lines())
        .filter_map(|l| l.strip_prefix("| `src/")?.split('`').next())
        .collect();
    let files: Vec<String> = rust_sources().into_iter().map(|(n, _)| n).collect();
    let missing: Vec<&String> = (files.iter())
        .filter(|f| !rows.contains(&f.as_str()))
        .collect();
    let gone: Vec<&&str> = (rows.iter())
        .filter(|r| !files.iter().any(|f| f == *r))
        .collect();
    assert!(
        missing.is_empty() && gone.is_empty(),
        "AGENTS.md's code layout lacks {missing:?} and names {gone:?}, which are not in src/"
    );
}

/// Each plain name in backticks in a row of AGENTS.md's code layout is defined in that row's
/// file, is in one of its string literals (a program's name), or is a feature in Cargo.toml.
/// So a function moved to another module takes its name to that module's row too, even when
/// the old module still calls it. Paths, macros and options in backticks are left out.
#[test]
fn agents_md_names_what_each_module_has() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let agents = std::fs::read_to_string(root.join("AGENTS.md")).unwrap();
    let cargo = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    let feature = |n: &str| cargo.contains(&format!("\n{n} = ["));
    let plain = |t: &&str| {
        t.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    let words = |t: &str| -> Vec<String> {
        (t.split(|c: char| !c.is_ascii_alphanumeric() && c != '_'))
            .filter(|w| !w.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let kinds = [
        "fn", "struct", "enum", "type", "const", "static", "trait", "mod",
    ];
    let mut wrong = Vec::new();
    for row in agents.lines() {
        let Some((file, rest)) = row.strip_prefix("| `src/").and_then(|r| r.split_once('`')) else {
            continue;
        };
        let text = source(file);
        let code = words(&code_lines(&text).join("\n"));
        let mut known: std::collections::HashSet<String> = (code.windows(2))
            .filter(|w| kinds.contains(&w[0].as_str()) || w[0] == "macro_rules")
            .map(|w| w[1].clone())
            .collect();
        // The words inside string literals, outside comments: a program's name, say.
        for line in text.lines().filter(|l| !l.trim_start().starts_with("//")) {
            known.extend(line.split('"').skip(1).step_by(2).flat_map(words));
        }
        let names = rest.split('`').skip(1).step_by(2).filter(plain);
        wrong.extend(
            names
                .filter(|n| !known.contains(*n) && !feature(n))
                .map(|n| format!("{file}: {n}")),
        );
    }
    assert!(
        wrong.is_empty(),
        "AGENTS.md's code layout names what is not defined in that file:\n{}",
        wrong.join("\n")
    );
}

/// A panic is caught only by `threads::catch_panic`, the one function whose doc says why. A worker
/// that caught panics its own way once forgot it, and a broken file ended the thread. Eight
/// commits added the catch one path at a time. A thread's `join()` whose error is swallowed
/// catches a panic too, and drops the thread's work without a word, so it passes the panic on
/// (`resume_unwind` or `.unwrap()`) or hands it to its caller (`?`). Only those spellings pass,
/// because a list of the ways to drop the error always missed one.
#[test]
fn panics_are_caught_in_one_place() {
    let mut wrong = Vec::new();
    for (file, text) in rust_sources() {
        if !file.ends_with("threads.rs") {
            wrong.extend((panics_caught_in(&text).iter()).map(|line| format!("{file}:{line}")));
        }
    }
    assert!(
        wrong.is_empty(),
        "a panic caught outside threads::catch_panic (use it, or resume_unwind a join's \
         error):\n{}",
        wrong.join("\n")
    );
}

/// Returns the lines of `text` outside the tests that catch a panic: `catch_unwind`, or a
/// thread's `join()` whose error is not passed on. A join passes it on only when `?`,
/// `.unwrap()` or `.expect(` follows it, also on the next line, or when the rest of its
/// statement calls `resume_unwind`. Every other spelling counts, because there are many ways to
/// drop an error (`.ok()`, `let Ok(…) = … else`, `match`, `.is_ok()`, `.into_iter()`).
fn panics_caught_in(text: &str) -> Vec<usize> {
    const PASSED_ON: [&str; 3] = ["?", ".unwrap()", ".expect("];
    let lines = code_lines(text);
    let end = tests_start(&lines);
    let mut found = Vec::new();
    for (i, line) in lines[..end].iter().enumerate() {
        let is_caught = line.split_once(".join()").is_some_and(|(_, after)| {
            let next = lines.get(i + 1).map_or("", |l| l.trim());
            let after = Some(after.trim()).filter(|a| !a.is_empty()).unwrap_or(next);
            let statement = statement_text(&lines[..end], i);
            let rest = statement.split_once(".join()").map_or("", |(_, rest)| rest);
            !(PASSED_ON.iter().any(|s| after.starts_with(s)) || rest.contains("resume_unwind"))
        });
        if line.contains("catch_unwind") || is_caught {
            found.push(i + 1);
        }
    }
    found
}

/// Returns the text of the statement that starts on line `first` of `lines`: that line, the
/// lines indented deeper after it, and the closing brace that ends them.
fn statement_text(lines: &[String], first: usize) -> String {
    let indent = |l: &str| l.len() - l.trim_start().len();
    let start = indent(&lines[first]);
    let mut text = lines[first].clone();
    for line in &lines[first + 1..] {
        if !line.trim().is_empty() && indent(line) <= start {
            if line.trim_start().starts_with('}') {
                text.push_str(line);
            }
            break;
        }
        text.push_str(line);
    }
    text
}

/// The check above finds `catch_unwind` and every join that does not pass its error on, in
/// each way an error is dropped. It leaves a join that passes the panic on, one in the tests,
/// and a `join` of text.
#[test]
fn finds_a_panic_caught() {
    let text = "fn a() {\n    std::panic::catch_unwind(f);\n    \
        workers.flat_map(|w| w.join().unwrap_or_default());\n    let _ = feed.join();\n    \
        handle\n        .join()\n        .ok();\n    \
        p.join().unwrap_or_else(|e| std::panic::resume_unwind(e));\n    \
        let text = parts.join(\", \");\n    drop(h.join());\n    \
        workers.flat_map(|w| w.join().unwrap_or_else(|_| Vec::new()));\n    \
        if let Ok(part) = h.join() {\n        all.extend(part);\n    }\n    \
        match h.join() {\n        Ok(p) => p,\n        Err(_) => Vec::new(),\n    };\n    \
        match h.join() {\n        Ok(p) => p,\n        Err(e) => resume_unwind(e),\n    };\n    \
        let parts = match h.join() {\n        Ok(p) => p,\n        Err(_) => Vec::new(),\n    \
        };\n    let Ok(part) = h.join() else {\n        continue;\n    };\n    \
        } else if let Ok(part) = h.join() {\n    \
        workers.flat_map(|w| w.join().into_iter().flatten());\n    if h.join().is_ok() {\n    \
        let part = h.join()?;\n    let part = h.join().unwrap();\n    \
        let part = h\n        .join()\n        .unwrap();\n\
        }\nmod tests {\n    fn t() {\n        h.join().ok();\n    }\n}\n";
    let found = panics_caught_in(text);
    assert_eq!(found, [2, 3, 4, 6, 10, 11, 12, 15, 23, 27, 30, 31, 32]);
}

/// A decoded image is turned by its EXIF orientation only after it is shrunk to the size shown
/// (`decode::shrunk_upright`), or where the whole image is wanted. A thumbnail of a 50 MP 16-bit
/// TIFF with orientation 6 turned whole held two full buffers in each thumbnail thread.
/// `decode::load_icc` turns the whole image, so it is watched too: a new path that called it and
/// shrank after would turn the whole image as well.
#[test]
fn images_are_turned_after_they_shrink() {
    // The scaled JPEG is at its scaled size already, and the others are for a whole image (Save
    // As, a turned save, an exported frame, a RAW preview shown near its size).
    const HOMES: &[(&str, &[(&str, &str)])] = &[
        (
            "apply_orientation(",
            &[
                ("images/decode.rs", "shrunk_upright"),
                ("images/decode.rs", "scaled"),
                ("images/decode.rs", "load_icc"),
                ("images/raw.rs", "load_data"),
            ],
        ),
        (
            "load_icc(",
            &[
                ("images/decode.rs", "load_page"),
                ("images/save.rs", "save_rotated"),
            ],
        ),
    ];
    let wrong = outside_their_homes(HOMES);
    assert!(
        wrong.is_empty(),
        "an image turned before it is shrunk (use decode::shrunk_upright):\n{}",
        wrong.join("\n")
    );
}

/// A process-wide copy of a setting is written only by its setter, so no place that changes the
/// setting can leave the copy behind. A setting saved but not in effect until a restart was
/// fixed three times for the user's own keys.
#[test]
fn copies_of_settings_have_one_writer() {
    const WRITERS: &[(&str, &[(&str, &str)])] =
        &[("USER_KEYS.write()", &[("ui/actions.rs", "use_user_keys")])];
    // The atomic copies, each written by `store`, `swap`, a `fetch_` or a `compare_exchange`.
    const ATOMICS: &[(&str, &[(&str, &str)])] = &[
        (
            "MAX_ANIMATION_BYTES.",
            &[("images/decode.rs", "set_animation_limit")],
        ),
        (
            "VIEW_FILTER.",
            &[
                ("images/colour.rs", "set_up_colour"),
                ("ui/menus.rs", "set_view_filter"),
            ],
        ),
        (
            "COLOR_MANAGEMENT.",
            &[
                ("images/colour.rs", "set_up_colour"),
                ("ui/menus.rs", "set_color_management"),
            ],
        ),
    ];
    let writes: Vec<(String, &[(&str, &str)])> = (ATOMICS.iter())
        .flat_map(|(copy, home)| {
            ["store(", "swap(", "fetch_", "compare_exchange"].map(|w| (format!("{copy}{w}"), *home))
        })
        .collect();
    let mut homes: Vec<(&str, &[(&str, &str)])> = WRITERS.to_vec();
    homes.extend(writes.iter().map(|(write, home)| (write.as_str(), *home)));
    let wrong = outside_their_homes(&homes);
    assert!(
        wrong.is_empty(),
        "a copy of a setting written by hand:\n{}",
        wrong.join("\n")
    );
    // A copy written by another method than `store` is found too.
    let planted = "fn a() {\n    VIEW_FILTER.swap(1, Relaxed);\n    \
        COLOR_MANAGEMENT.fetch_xor(true, Relaxed);\n    VIEW_FILTER.load(Relaxed);\n}\n";
    assert_eq!(
        outside_homes_in("ui/file_list.rs", planted, &homes),
        [
            "ui/file_list.rs:2 (VIEW_FILTER.swap()",
            "ui/file_list.rs:3 (COLOR_MANAGEMENT.fetch_)"
        ]
    );
}

/// The remote control setting decides only at start whether a window listens for `--remote`
/// without a name. After that, `remote::listening()` says whether it listens, as a named window
/// or a kiosk listens with the setting off. Before, a window marked itself as the one used last
/// only with the setting on, so plain `--remote` reached another window than the one in use.
#[test]
fn whether_a_window_listens_is_asked_in_one_place() {
    const HOMES: &[(&str, &[(&str, &str)])] = &[(
        "remote_control",
        &[
            ("main.rs", "with_config"),
            ("files/config.rs", ""),
            ("ui/preferences.rs", "window_settings"),
            ("support/test_support.rs", "app_on"),
        ],
    )];
    let wrong = outside_their_homes(HOMES);
    assert!(
        wrong.is_empty(),
        "the remote control setting read where remote::listening() is meant:\n{}",
        wrong.join("\n")
    );
}

/// A notice is given only through `Notice::give`, which gives it a number of its own, so a
/// screen reader reads out a notice given again with the same text. Before, the screen reader's
/// code told a notice given again by its text's address, and the second of two notices given
/// within one frame often got the address of the first one back, so it was not read out.
#[test]
fn notices_are_given_through_their_setter() {
    let wrong = outside_their_homes(&[(".notice =", &[]), ("&mut self.notice", &[])]);
    assert!(
        wrong.is_empty(),
        "give a notice with notice.give(…) or take it away with notice.clear():\n{}",
        wrong.join("\n")
    );
}

/// A Close is sent only by `ask_to_close`, and the answers that tell of a failed write (the
/// save error, the notices channel and the turns saved) are taken only in `take_saves`, which
/// `guard_close` calls before it lets a close through. A close that
/// waited for keywords to be written closed the window in the pass that took their failure, so
/// the user never saw that the keywords were lost. A Close sent from another place, or a failed
/// write taken in another place, could skip that again.
///
/// A failure must also reach the window as a failed write, not as a plain notice, which a
/// close does not wait for. Work in `run_in_background` answers with a `Told`, so it says which
/// its answer is, and a function that hands work to a writer thread holds no `notice_sender`,
/// so the writer tells a failure through `save_failed`. A function that starts a job or other
/// writing work hands its `notice_sender` only to `threads::told_work`. A crop, Save As, an
/// Undo and Rename Collection sent their failures as plain notices, and so did Print to File
/// and Copy for Sharing, and a close that waited closed over them.
#[test]
fn a_close_takes_the_failed_writes_first() {
    const HOMES: &[(&str, &[(&str, &str)])] = &[
        (
            "ViewportCommand::Close",
            &[("ui/window.rs", "ask_to_close")],
        ),
        ("take_save_error(", &[("ui/workers.rs", "take_saves")]),
        ("take_lock_warning(", &[("ui/workers.rs", "take_saves")]),
        ("take_saved(", &[("ui/workers.rs", "take_saves")]),
        ("self.notices.", &[("ui/workers.rs", "take_saves")]),
    ];
    let mut wrong = outside_their_homes(HOMES);
    for (file, text) in rust_sources() {
        wrong.extend(notices_beside_writers(&file, &text));
    }
    assert!(
        wrong.is_empty(),
        "send a Close with ask_to_close, take a failed write in take_saves, and tell a writer \
         thread's failure with save_failed:\n{}",
        wrong.join("\n")
    );
    let file_ops = include_str!("../files/file_ops.rs");
    assert!(file_ops.contains("work: impl FnOnce() -> Option<Told> + Send + 'static,"));
}

/// The check above finds a function that hands work to a writer thread and holds a
/// `notice_sender` too, as Rename Collection did.
#[test]
fn finds_a_notice_sender_beside_a_writer() {
    let text = "impl A {\n    fn a(&self) {\n        let n = self.notice_sender.clone();\n        \
        write_later_in_user_folders(move || drop(n.send(e)));\n    }\n    fn b(&self) {\n        \
        write_later(move || save_failed(e));\n    }\n    fn c(&self) {\n        \
        let n = self.notice_sender.clone();\n    }\n}\n";
    assert_eq!(notices_beside_writers("x.rs", text), ["x.rs: a"]);
}

/// Returns the functions in `text`, the source of `file`, that hand work to a writer thread
/// (`write_later` or `write_later_in_user_folders`) or start writing work (`WRITING_WORK`), and
/// also hold a `notice_sender` that is not handed to `told_work`, by which that work could send
/// a failure as a plain notice.
fn notices_beside_writers(file: &str, text: &str) -> Vec<String> {
    let lines = code_lines(text);
    let end = tests_start(&lines);
    let (mut function, mut writes, mut notices) = (String::new(), false, false);
    let mut wrong = Vec::new();
    // A last line that starts no function ends the last one, so it is checked too.
    let last = "fn end".to_owned();
    for line in lines[..end].iter().chain([&last]) {
        if let Some(name) = function_name(line) {
            if writes && notices {
                wrong.push(format!("{file}: {function}"));
            }
            (function, writes, notices) = (name.to_owned(), false, false);
        }
        writes |= mentions(line, "write_later(") || mentions(line, "write_later_in_user_folders(");
        // A function that starts a job or work that counts as writing hands its sender only to
        // `told_work`, so the work tells through a `Told`, which says whether it is a failure.
        writes |= WRITING_WORK.iter().any(|w| line.contains(w));
        notices |= line.contains("notice_sender") && !line.contains("told_work(");
    }
    wrong
}

/// A thread that writes, one that holds a job, the count of writing or an Undo on the way, runs
/// its work through `threads::told_work`, which catches a panic and tells it as a failed write,
/// and keeps the count of writing until it has told. Delete, Copy and Move, Rename Many, Copy for
/// Sharing and Print ran in threads of their own, and a panic in one ended it with nothing told:
/// no report, no Undo, and a close that waited for it went ahead. A writer's `Took::Gone` then
/// needs nothing more, as the thread told its end itself.
#[test]
fn writing_threads_tell_what_they_did() {
    let mut wrong = Vec::new();
    for (file, text) in rust_sources() {
        wrong.extend(untold_writing_threads(&file, &text));
    }
    assert!(
        wrong.is_empty(),
        "run a thread that writes through run_in_background or threads::told_work:\n{}",
        wrong.join("\n")
    );
}

/// The checks above find a thread that holds a job and tells nothing, and a function with a job
/// that holds a `notice_sender`, and leave a thread that reads alone.
#[test]
fn finds_a_writing_thread_that_tells_nothing() {
    // Through `app`, not `self`, so the file operations' contract does not take these for
    // operations of its own.
    let text = "impl A {\n    fn a(app: &App) {\n        let job = app.start_job(x);\n        \
        std::thread::spawn(move || drop(job));\n    }\n    fn b(app: &App) {\n        \
        let w = app.start_job_of(x);\n        \
        std::thread::spawn(told_work(app.notice_sender.clone(), c, w, work));\n    }\n    \
        fn c(app: &App) {\n        std::thread::spawn(move || read());\n    }\n    \
        fn d(app: &App) {\n        let n = app.notice_sender.clone();\n        \
        app.run_in_background(ctx, Vec::new(), move || drop(n.send(e)));\n    }\n    \
        fn e(app: &App) {\n        let tx = app.notice_sender.clone();\n        \
        app.write_sidecars(ctx, a, f, move |p, _| drop(tx.send(e)), r);\n    }\n}\n";
    assert_eq!(untold_writing_threads("x.rs", text), ["x.rs: a"]);
    assert_eq!(notices_beside_writers("x.rs", text), ["x.rs: d", "x.rs: e"]);
}

/// Returns the functions in `text`, the source of `file`, that start writing work
/// (`WRITING_WORK`) and spawn a thread that does not run through `told_work`.
fn untold_writing_threads(file: &str, text: &str) -> Vec<String> {
    let lines = code_lines(text);
    let end = tests_start(&lines);
    let (mut function, mut writes, mut untold) = (String::new(), false, false);
    let mut wrong = Vec::new();
    // A last line that starts no function ends the last one, so it is checked too.
    let last = "fn end".to_owned();
    for line in lines[..end].iter().chain([&last]) {
        if let Some(name) = function_name(line) {
            if writes && untold {
                wrong.push(format!("{file}: {function}"));
            }
            (function, writes, untold) = (name.to_owned(), false, false);
        }
        writes |= WRITING_WORK.iter().any(|w| line.contains(w));
        untold |= line.contains("thread::spawn(") && !line.contains("told_work(");
    }
    wrong
}

/// A setting that has a setter (an App `set_…` method, or a View action's `toggle_setting`) is
/// changed only through that setter, in every file, so each place that changes it does what
/// the menu does besides. Preferences wrote the names under the thumbnails directly and skipped
/// the scroll to the shown image, and Keywords and Comment turned the info panel on the same way.
/// Each setting has one setter, and a setter writes no other's setting, as it would skip what
/// that one does.
#[test]
fn settings_change_through_their_setters() {
    let sources = rust_sources();
    // Each setting that has a setter, with the file and the name of every setter that writes it.
    let mut setters: std::collections::HashMap<String, Vec<(&str, String)>> = Default::default();
    for (file, text) in &sources {
        for (function, _, field) in config_writes(text) {
            // A on a stepped animation plays it on and leaves the setting, which a checkbox in
            // Preferences must not.
            let toggle = function == "toggle_setting" && field != "animate";
            if !function.starts_with("set_") && !toggle {
                continue;
            }
            let theirs = setters.entry(field).or_default();
            let setter = (file.as_str(), function);
            if !theirs.contains(&setter) {
                theirs.push(setter);
            }
        }
    }
    for name in [
        "thumb_size",
        "grid_names",
        "thumbnails",
        "icon_grid",
        "show_subfolders",
        "info_panel",
    ] {
        assert!(setters.contains_key(name), "{name} has no setter found");
    }
    // One setter to a setting, so a new `set_…` that writes another's setting is seen too.
    let mut shared: Vec<String> = (setters.iter())
        .filter(|(_, s)| s.len() > 1)
        .map(|(field, s)| format!("{field}: {s:?}"))
        .collect();
    shared.sort();
    assert!(
        shared.is_empty(),
        "settings written by more than one setter:\n{}",
        shared.join("\n")
    );
    // Two places write a setting on purpose. A folder opened with its subfolders is listed with
    // them, where the setter would first list the folder before it again. Compare shows one
    // pane, where `set_layout` would turn Compare off.
    const MEANT: &[(&str, &str)] = &[
        ("ui/dialogs.rs", "receive_picked"),
        ("viewer/view.rs", "toggle_compare"),
    ];
    let mut wrong = Vec::new();
    // The settings file's own reader fills the fields it reads.
    for (file, text) in sources.iter().filter(|(f, _)| f != "files/config.rs") {
        for (function, line, field) in config_writes(text) {
            let Some(theirs) = setters.get(&field) else {
                continue;
            };
            let own = theirs.contains(&(file.as_str(), function.clone()));
            if !own && !MEANT.contains(&(file.as_str(), function.as_str())) {
                wrong.push(format!("{file}:{line} ({field} in {function})"));
            }
        }
    }
    // The settings replaced all at once skip every setter.
    for (file, text) in sources.iter().filter(|(f, _)| f != "files/config.rs") {
        wrong.extend((whole_config_writes(text).iter()).map(|line| format!("{file}:{line} (all)")));
    }
    assert!(
        wrong.is_empty(),
        "set through their setters instead:\n{}",
        wrong.join("\n")
    );
}

/// Returns the lines of `text` outside the tests that replace every setting at once:
/// `self.config` assigned, or given to `mem::replace`, `swap` or `take`.
fn whole_config_writes(text: &str) -> Vec<usize> {
    let lines = code_lines(text);
    let end = tests_start(&lines);
    let mut found = Vec::new();
    for (i, line) in lines[..end].iter().enumerate() {
        let assigned = (line.match_indices("self.config")).any(|(at, name)| {
            let after = line[at + name.len()..].trim_start();
            after.starts_with('=') && !after.starts_with("==") && !after.starts_with("=>")
        });
        let swapped = [
            "replace(&mut self.config",
            "swap(&mut self.config",
            "take(&mut self.config",
        ]
        .iter()
        .any(|t| line.contains(t));
        if assigned || swapped {
            found.push(i + 1);
        }
    }
    found
}

/// The check above finds the settings assigned or swapped all at once, and leaves one setting
/// written, a comparison and a binding.
#[test]
fn finds_every_setting_replaced_at_once() {
    let text = "fn a(&mut self) {\n    self.config = edited;\n    \
        *self.config = edited;\n    std::mem::swap(&mut self.config, &mut other);\n    \
        self.config.thumbnails = true;\n    if self.config == old {}\n    \
        let c = &mut self.config;\n}\n";
    assert_eq!(whole_config_writes(text), [2, 3, 4]);
}

/// Every setting of `config!` has a control in Preferences, so a user can find it there, or is
/// listed here with the place that changes it instead. A setting counts as having a control
/// when Preferences' drawing code gives it to a call, as a control is drawn (`gives_to_a_call`),
/// not when it is only read, in a condition or in the tests.
#[test]
fn every_setting_has_a_control_in_preferences() {
    const ELSEWHERE: &[(&str, &str)] = &[
        ("layout", "View → Layout"),
        ("slideshow_one_image_at_a_time", "View → Layout"),
        ("layout_names", "View → Layout"),
        ("fit_mode", "View → Zoom"),
        ("view_filter", "View → Filter"),
        ("sort_manager", "View → Sort Manager"),
        ("sort_move", "the sort manager's Copy and Move"),
        ("sort", "the status bar's sort menu"),
        ("sort_ascending", "the status bar's sort menu"),
        ("sort_case_sensitive", "the status bar's sort menu"),
        ("list_images", "the status bar's Types menu"),
        ("list_raw", "the status bar's Types menu"),
        ("show_hidden", "View → Show Hidden Files"),
        ("search_case_sensitive", "the search field's Case box"),
        ("projector_pointer", "Window → Projector"),
        ("lightroom_keywords", "the Import Lightroom Catalog dialog"),
        ("print_printer", "the print dialog"),
        ("print_paper", "the print dialog"),
        ("print_orientation", "the print dialog"),
        ("print_caption", "the print dialog"),
        ("print_template", "the print dialog"),
        ("print_per_page", "the print dialog"),
        ("print_margin", "the print dialog"),
        ("print_scale", "the print dialog"),
        ("print_fill", "the print dialog"),
        ("print_header", "the print dialog"),
        ("print_footer", "the print dialog"),
        ("print_two_sided", "the print dialog"),
        (
            "app_name_first",
            "retired in 0.76.0, read once into window_title",
        ),
    ];
    let preferences = code_lines(&source("ui/preferences.rs")).join("\n");
    let drawing = preferences.split("#[cfg(test)]").next().unwrap_or_default();
    let has_control = |name: &str| gives_to_a_call(drawing, name);
    let text = crate::files::config::Config::default().to_text();
    let settings: Vec<&str> = (text.lines())
        .filter_map(|line| Some(line.split_once('=')?.0))
        .collect();
    let without: Vec<&&str> = (settings.iter())
        .filter(|name| !has_control(name) && !ELSEWHERE.iter().any(|(n, _)| n == *name))
        .collect();
    assert!(without.is_empty(), "no control in Preferences: {without:?}");
    let listed_wrongly: Vec<&str> = (ELSEWHERE.iter())
        .map(|(name, _)| *name)
        .filter(|name| !settings.contains(name) || has_control(name))
        .collect();
    assert!(
        listed_wrongly.is_empty(),
        "not to be listed: {listed_wrongly:?}"
    );
}

/// Whether `code` gives the field `name` to a call, as Preferences gives a setting to the
/// control that changes it: `.name` inside a call's brackets, or read into a local by `let`
/// that a call in the same block then takes. A field only read in a condition is not given.
fn gives_to_a_call(code: &str, name: &str) -> bool {
    let field = format!(".{name}");
    (code.match_indices(&field)).any(|(at, _)| {
        let after = &code[at + field.len()..];
        if after.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
            return false;
        }
        if innermost_bracket(&code[..at]) == Some('(') {
            return true;
        }
        let line = &code[code[..at].rfind('\n').map_or(0, |i| i + 1)..at];
        let local = (line.trim_start().strip_prefix("let ")).and_then(|rest| {
            rest.trim_start_matches("mut ")
                .split([' ', ':', '='])
                .next()
        });
        local.is_some_and(|local| !local.is_empty() && a_call_takes(after, local))
    })
}

/// Returns the bracket that is still open at the end of `code`: '(' inside a call's
/// arguments, '{' in a block, or `None` outside every bracket.
fn innermost_bracket(code: &str) -> Option<char> {
    let mut open = Vec::new();
    for c in code.chars() {
        match c {
            '(' | '[' | '{' => open.push(c),
            ')' | ']' | '}' => {
                open.pop();
            }
            _ => {}
        }
    }
    open.pop()
}

/// Whether a call takes the word `local` in `code` before the block that `code` starts in ends.
fn a_call_takes(code: &str, local: &str) -> bool {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut open = Vec::new();
    for (at, c) in code.char_indices() {
        match c {
            '(' | '[' | '{' => open.push(c),
            ')' | ']' | '}' if open.pop().is_none() => return false,
            _ if open.last() == Some(&'(')
                && code[at..].starts_with(local)
                && !code[..at].ends_with(is_word)
                && !code[at + local.len()..].starts_with(is_word) =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

/// The Preferences rule counts a setting given to a control, also through a local, and not one
/// only read in a condition, as `.name` anywhere in the file once did.
#[test]
fn a_setting_counts_as_a_control_only_when_a_call_takes_it() {
    let code = "fn draw(&mut self) {\n    if self.config.only_read {\n        x();\n    }\n    \
         changed |= checkbox(ui, &mut c.given, t).changed();\n    \
         let on = self.config.through;\n    self.toggle_box(ui, on, Action::A, t);\n    \
         let unused = self.config.read_into;\n    if unused {\n        y();\n    }\n}\n";
    assert!(!gives_to_a_call(code, "only_read"));
    assert!(gives_to_a_call(code, "given"));
    assert!(gives_to_a_call(code, "through"));
    assert!(!gives_to_a_call(code, "read_into"));
    assert!(!gives_to_a_call(code, "giv"));
}

/// The setter test sees a setting written however it is written: assigned, added to, or taken
/// as `&mut`, also through `&mut self.config` bound to any name, but not compared or read.
#[test]
fn a_setting_is_seen_written_however_it_is_written() {
    let code = "fn set_a(&mut self) {\n    if on {\n        x();\n    }\n    self.config.a = 1;\n}\n\
         fn b(&mut self) {\n    let mut cfg = &mut self.config;\n    cfg.b += \"}\";\n    \
         cfg.d == 2;\n    let f = &mut cfg.f;\n    x.g = 3;\n    self.config.h -= 1;\n}\n";
    let found = config_writes(code);
    let found: Vec<_> = found
        .iter()
        .map(|(f, l, s)| (f.as_str(), *l, s.as_str()))
        .collect();
    assert_eq!(
        found,
        [
            ("set_a", 5, "a"),
            ("b", 9, "b"),
            ("b", 11, "f"),
            ("b", 13, "h")
        ]
    );
    // The spellings that slipped through before: a tuple of places, a part of a setting, a
    // method that changes it, also on a chain that rustfmt broke over lines, and a pattern
    // bound to the settings. A read through a part or a method is no write.
    let code = "fn c(&mut self) {\n    (self.config.i, self.config.j) = (1, 2);\n    \
         self.config.k.0 = 3;\n    self.config.l[0] = 4;\n    self.config.m.push(5);\n    \
         self.config\n        .n\n        .0\n        .retain(|x| x > 0);\n    \
         self.config.o.clone_from(&p);\n    let Config { q, ref mut r, .. } = &mut self.config;\n    \
         let s = self.config.s.0.len();\n    if self.config.t.contains(&u) {}\n    \
         (a, b) = (self.config.v, 1);\n}\n";
    let found = config_writes(code);
    let found: Vec<_> = found
        .iter()
        .map(|(f, l, s)| (f.as_str(), *l, s.as_str()))
        .collect();
    assert_eq!(
        found,
        [
            ("c", 2, "i"),
            ("c", 2, "j"),
            ("c", 3, "k"),
            ("c", 4, "l"),
            ("c", 5, "m"),
            ("c", 6, "n"),
            ("c", 10, "o"),
            ("c", 11, "q"),
            ("c", 11, "r"),
        ]
    );
}

/// Returns each place outside the tests in `text` where a setting is written, as the function,
/// the line's number and the setting. A setting is written when it is assigned (`=`, `+=` and
/// the like), also in a tuple of places, when one of its parts is (`.0 =`, `[i] =`), when a
/// method that changes it is called on it or on a part (`.push(`, `.retain(`, `.clone_from(`),
/// or when it is taken as `&mut`. It is found as `self.config.X`, through a name that the
/// function bound to `&mut self.config`, and as a field of a pattern bound to it. A line that
/// starts with `.` is read with the line before it, as rustfmt breaks a long chain of calls.
fn config_writes(text: &str) -> Vec<(String, usize, String)> {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    // The methods of the standard library's types that change their value (`&mut self`).
    const CHANGING: &[&str] = &[
        "append",
        "as_mut",
        "clear",
        "clone_from",
        "dedup",
        "drain",
        "entry",
        "extend",
        "extend_from_slice",
        "fill",
        "get_mut",
        "get_or_insert",
        "get_or_insert_with",
        "insert",
        "insert_str",
        "iter_mut",
        "pop",
        "push",
        "push_str",
        "remove",
        "replace",
        "resize",
        "retain",
        "retain_mut",
        "sort",
        "sort_by",
        "sort_by_key",
        "sort_unstable",
        "split_off",
        "swap",
        "take",
        "truncate",
    ];
    let lines = code_lines(text);
    // Each line, with the lines that go on its chain of calls joined to it.
    let mut joined: Vec<(usize, String)> = Vec::new();
    for (i, line) in lines[..tests_start(&lines)].iter().enumerate() {
        match joined.last_mut() {
            Some((_, last)) if line.trim_start().starts_with('.') => last.push_str(line.trim()),
            _ => joined.push((i, line.clone())),
        }
    }
    let mut function = String::new();
    let mut prefixes = Vec::new();
    let mut writes = Vec::new();
    for (i, line) in &joined {
        // A top-level line starts a new item, so its bindings end there.
        if line.starts_with(|c: char| c.is_alphabetic()) {
            function.clear();
            prefixes = vec!["self.config.".to_owned()];
        }
        if let Some(name) = function_name(line) {
            function = name.to_owned();
            prefixes = vec!["self.config.".to_owned()];
        }
        let bound = line.trim().strip_prefix("let ");
        if let Some(rest) = bound.and_then(|r| r.strip_suffix("= &mut self.config;")) {
            let rest = rest.strip_prefix("mut ").unwrap_or(rest);
            match rest.split_once('{') {
                // A pattern, as `Config { thumbnails, .. }`, writes each field it binds.
                Some((_, fields)) => {
                    for field in fields.split([',', '}']) {
                        let field = field.trim().trim_start_matches("ref ");
                        let field = field.trim_start_matches("mut ");
                        let name: String = field.chars().take_while(|&c| ident(c)).collect();
                        if !name.is_empty() {
                            writes.push((function.clone(), i + 1, name));
                        }
                    }
                }
                None => {
                    let name: String = rest.chars().take_while(|&c| ident(c)).collect();
                    prefixes.push(format!("{name}."));
                }
            }
        }
        for prefix in &prefixes {
            for (at, _) in line.match_indices(prefix.as_str()) {
                if line[..at].ends_with(|c: char| ident(c) || c == '.') {
                    continue;
                }
                let rest = &line[at + prefix.len()..];
                let name: String = rest.chars().take_while(|&c| ident(c)).collect();
                let after = parts_passed_over(&rest[name.len()..]).trim_start();
                let method: String = (after.strip_prefix('.').unwrap_or_default().chars())
                    .take_while(|&c| ident(c))
                    .collect();
                let changes = CHANGING.contains(&method.as_str())
                    && after[1 + method.len()..].starts_with('(');
                let written = is_assignment(after)
                    || changes
                    || line[..at].ends_with("&mut ")
                    || in_assigned_tuple(line, at);
                if !name.is_empty() && written {
                    writes.push((function.clone(), i + 1, name));
                }
            }
        }
    }
    writes
}

/// Passes over the fields and indexes at the start of `code` (`.0`, `.name`, `[i]`), which name
/// a part of the value before them. Returns the code after them, from a call or anything else.
fn parts_passed_over(mut code: &str) -> &str {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    loop {
        let trimmed = code.trim_start();
        if let Some(rest) = trimmed.strip_prefix('.') {
            let field = rest.len() - rest.trim_start_matches(ident).len();
            if field == 0 || rest[field..].starts_with('(') {
                return code;
            }
            code = &rest[field..];
        } else if let Some(close) = trimmed.strip_prefix('[').and_then(|r| r.find(']')) {
            code = &trimmed[close + 2..];
        } else {
            return code;
        }
    }
}

/// Whether `code` starts with an assignment (`=`, `+=` and the like), not with a comparison or
/// the arrow of a match's arm.
fn is_assignment(code: &str) -> bool {
    const ASSIGN: &[&str] = &[
        "=", "+=", "-=", "*=", "/=", "%=", "|=", "&=", "^=", "<<=", ">>=",
    ];
    ASSIGN.iter().any(|op| code.starts_with(op))
        && !code.starts_with("==")
        && !code.starts_with("=>")
}

/// Whether the place at `at` in `line` is inside a tuple of places that the line assigns to, as
/// in `(self.config.a, self.config.b) = (1, 2);`.
fn in_assigned_tuple(line: &str, at: usize) -> bool {
    let start = line.len() - line.trim_start().len();
    if !line[start..].starts_with('(') {
        return false;
    }
    let mut depth = 0;
    for (i, c) in line[start..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            let close = start + i;
            return at < close && is_assignment(line[close + 1..].trim_start());
        }
    }
    false
}

/// Returns each place outside the tests where a text of `homes` is written in code outside the
/// functions it is allowed in, as "file:line (text)". A function named "" allows the whole
/// file, and the text's own definition is allowed anywhere.
fn outside_their_homes(homes: &[(&str, &[(&str, &str)])]) -> Vec<String> {
    let mut wrong = Vec::new();
    for (file, text) in rust_sources() {
        wrong.extend(outside_homes_in(&file, &text, homes));
    }
    wrong
}

/// Returns the places in `text`, the source of `file`, that `outside_their_homes` reports.
fn outside_homes_in(file: &str, text: &str, homes: &[(&str, &[(&str, &str)])]) -> Vec<String> {
    let mut wrong = Vec::new();
    let lines = code_lines(text);
    let end = tests_start(&lines);
    let mut function = String::new();
    for (i, line) in lines[..end].iter().enumerate() {
        // A top-level line starts a new item, so a static after a home is not in the home.
        if line.starts_with(|c: char| c.is_alphabetic()) {
            function.clear();
        }
        if let Some(name) = function_name(line) {
            function = name.to_owned();
        }
        for (call, allowed) in homes {
            let at_home =
                (allowed.iter()).any(|(f, s)| file == *f && (s.is_empty() || function == *s));
            let defined = line.contains(&format!("fn {call}"));
            if mentions(line, call) && !at_home && !defined {
                wrong.push(format!("{file}:{} ({call})", i + 1));
            }
        }
    }
    wrong
}

/// Returns whether `line` holds `text`. A call (`name(`) is also found as a function value
/// (`.map(name)`, `f(name, x)`), and as renamed by a `use` (`use …::name as other;`), whose
/// calls by the new name would not hold `text`. A rename is found on any line, so also inside a
/// `use` whose braces span several lines.
fn mentions(line: &str, text: &str) -> bool {
    let Some(name) = text.strip_suffix('(').filter(|n| n.ends_with(is_word_char)) else {
        return line.contains(text);
    };
    let is_whole = |at: usize| !line[..at].ends_with(is_word_char);
    let is_use = without_qualifiers(line.trim_start()).starts_with("use ");
    line.contains(text)
        || (line.match_indices(name)).any(|(at, _)| {
            let after = &line[at + name.len()..];
            is_whole(at)
                && (after.starts_with(" as ") || (!is_use && after.starts_with([')', ','])))
        })
}

/// Returns whether `c` can be part of a name.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// A call is found also as a function value and as renamed by a `use`, also inside braces that
/// span several lines, but not in a `use` that keeps its name, nor in a longer name. Before, a
/// rename was found only on the line that starts with `use`.
#[test]
fn finds_a_call_by_another_spelling() {
    const HOMES: &[(&str, &[(&str, &str)])] = &[("replace_file(", &[("a.rs", "home")])];
    let text = "use crate::files::file_io::replace_file as write_own;\n\
        use crate::files::file_io::{replace_file, write_atomic};\nfn a() {\n    \
        paths.iter().map(file_io::replace_file);\n    try_all(replace_file, &paths);\n    \
        try_all(own_replace_file, &paths);\n    replace_files(p);\n}\n\
        use crate::files::file_io::{\n    write_atomic,\n    replace_file as write_own,\n};\n";
    let wrong = outside_homes_in("a.rs", text, HOMES);
    let lines: Vec<&str> = (wrong.iter())
        .map(|w| w.split(' ').next().unwrap())
        .collect();
    assert_eq!(lines, ["a.rs:1", "a.rs:4", "a.rs:5", "a.rs:11"]);
}

/// A home ends at the next top-level item. Before, a static or const after a home function
/// counted as inside it, so a call there passed unseen.
#[test]
fn a_home_ends_at_the_next_top_level_item() {
    const HOMES: &[(&str, &[(&str, &str)])] = &[("risky(", &[("a.rs", "home")])];
    let text = "fn home() {\n    risky();\n}\nstatic X: u8 = risky();\nconst Y: u8 = risky();\n\
        impl A {\n    fn home() {\n        risky();\n    }\n}\n";
    let wrong = outside_homes_in("a.rs", text, HOMES);
    assert_eq!(wrong, ["a.rs:4 (risky()", "a.rs:5 (risky()"]);
}

/// A file Glimt writes from an image (a turn saved, a crop, a copy, their sidecars) takes the
/// image's metadata only through `save::derived_metadata` and `save::derived_xmp`, which say what
/// such a file keeps. The EXIF thumbnail was cleared there, but the XMP's thumbnail and a PNG's
/// Photoshop resources were copied as they were, because each kind was decided apart. The readers
/// that give an image's XMP as it is are watched too, because a copy written from one would skip
/// what `derived_xmp` drops. The panel reads it in `xmp::embedded`. The EXIF block is read through
/// `exif::block_from_decoder`, which also sees a PNG's EXIF in a text chunk. Before, a turned or
/// copied PNG lost EXIF that only such a chunk held, because the save read the eXIf chunk alone. A
/// derived file takes its EXIF block from `exif::derived`, the panel's reader, and never from
/// `exif::block_from_decoder`, which reads only what the image crate opens. Before, a copy of a
/// HEIC, AVIF, JPEG XL or RAW photo lost the EXIF the panel showed. The orientation an image is
/// turned upright by is read through `exif::stored_orientation`, so the view and a turned or copied
/// file take the one the panel shows. The view once took the decoder's alone and showed a PNG with
/// its orientation in a text chunk unturned. The view's Adobe RGB check takes the same block as its
/// orientation, and the colour line reads it through `block_from_decoder` too, so a PNG with its
/// ColorSpace only in a text chunk is shown and named as Adobe RGB. A RAW's preview is a JPEG, read
/// on its own. A copy or an exported frame reads a page through `decode::load_page`, which gives
/// every page with its own ICC profile. Before, a later page of a TIFF was read without it. Every
/// image file Glimt writes is encoded by `save::encode`, which fits the pixels to the format.
/// Before, Export Frames wrote a float page as it was, which PNG refuses (`.write_to` is watched
/// for that).
#[test]
fn derived_files_take_metadata_in_one_place() {
    const HOMES: &[(&str, &[(&str, &str)])] = &[
        ("set_upright(", &[("images/save.rs", "derived_metadata")]),
        (
            "exif::derived(",
            &[
                ("images/save.rs", "derived_metadata"),
                ("images/save.rs", "shared_metadata"),
            ],
        ),
        (
            "exif::block_from_decoder(",
            &[
                ("images/decode.rs", "load_unturned"),
                ("images/colour.rs", "colour_source"),
            ],
        ),
        (
            ".exif_metadata()",
            &[
                ("metadata/exif.rs", "block_from_decoder"),
                ("images/raw.rs", "exif"),
            ],
        ),
        (
            "xmp_metadata(",
            &[
                ("images/save.rs", "derived_metadata"),
                ("metadata/xmp.rs", "embedded"),
            ],
        ),
        (
            ".orientation()",
            &[
                ("metadata/exif.rs", "stored_orientation"),
                ("images/raw.rs", "unturned"),
            ],
        ),
        (
            "page_of(",
            &[
                ("images/decode.rs", "decode"),
                ("images/decode.rs", "load_page"),
            ],
        ),
        (".write_to(", &[("images/save.rs", "lossy_webp")]),
        ("png_with_xmp(", &[("images/save.rs", "encode")]),
        ("set_xmp_orientation(", &[("images/save.rs", "derived_xmp")]),
        ("set_exif_metadata(", &[("images/save.rs", "encode")]),
        (
            "own_sidecar(",
            &[
                ("metadata/xmp.rs", ""),
                ("images/save.rs", "save_copy_with_sidecar"),
                ("images/save.rs", "derive_sidecar"),
                ("images/save.rs", "shared_metadata"),
            ],
        ),
    ];
    let wrong = outside_their_homes(HOMES);
    assert!(
        wrong.is_empty(),
        "metadata copied to a derived file around save::derived_metadata or derived_xmp:\n{}",
        wrong.join("\n")
    );
}

/// An image written again and its sidecar change together, through `save::replace_with_sidecar`,
/// which changes the sidecar only once the image's own write can no longer fail. Before, a turn
/// saved changed the sidecar first, so a read-only image kept its pixels while its sidecar said
/// it was turned.
#[test]
fn an_image_and_its_sidecar_change_together() {
    const HOMES: &[(&str, &[(&str, &str)])] = &[
        (
            "derive_sidecar(",
            &[("images/save.rs", "replace_with_sidecar")],
        ),
        (
            "replace_existing(",
            &[
                ("files/file_io.rs", ""),
                ("images/save.rs", "replace_with_sidecar_locked"),
            ],
        ),
    ];
    let wrong = outside_their_homes(HOMES);
    assert!(
        wrong.is_empty(),
        "an image or its sidecar written around save::replace_with_sidecar:\n{}",
        wrong.join("\n")
    );
}

/// The keywords, comment, alt text and stars in a sidecar of the user's are written only in the
/// work handed to `write_sidecars`, which holds the one rule for when such a write runs beside
/// other work on its file. The homes are the functions that hand that work over. Before, the
/// rule was kept by the operations and the turn, and a keyword written while Rename Many swapped
/// two names landed on the other photo.
#[test]
fn sidecars_are_written_through_write_sidecars() {
    const IN_WRITE_SIDECARS: &[(&str, &str)] = &[
        ("metadata/xmp.rs", ""),
        ("ui/info_panel.rs", "save_metadata_edit"),
        ("ui/info_panel.rs", "set_keyword_on_files"),
        ("ui/info_panel.rs", "add_keywords_to_selected"),
        ("ui/actions.rs", "apply_mark"),
        ("metadata/lightroom.rs", "write_imported"),
    ];
    const HOMES: &[(&str, &[(&str, &str)])] = &[
        ("write_changed(", IN_WRITE_SIDECARS),
        ("write_alt_text(", IN_WRITE_SIDECARS),
        ("add_keywords(", IN_WRITE_SIDECARS),
        ("tag_file(", IN_WRITE_SIDECARS),
        ("add_tags(", IN_WRITE_SIDECARS),
        ("write_xmp(", IN_WRITE_SIDECARS),
        ("update_sidecar(", &[("metadata/marks.rs", "write_xmp")]),
        (
            "update_locked_sidecar(",
            &[
                ("metadata/xmp.rs", ""),
                ("images/save.rs", "derive_sidecar"),
            ],
        ),
        (
            "sidecar_writer(",
            &[("files/file_ops.rs", "write_in_sidecar_thread")],
        ),
    ];
    // A writer is found however its call is spelled: by its name after a `use`, renamed in a
    // `use` that spans several lines, and under a lock taken by hand. Before, `add_keywords(`
    // was found only as `xmp::add_keywords(`, and `update_locked_sidecar(` not at all.
    let around = "use crate::metadata::xmp::add_keywords;\nfn a(p: &Path) {\n    \
        add_keywords(p, &k);\n    let lock = xmp::lock_sidecar(p);\n    \
        xmp::update_locked_sidecar(&lock, p, true, |x| Some(x));\n}\n\
        use crate::metadata::xmp::{\n    add_keywords as tag,\n    read_sidecar,\n};\n";
    let found = outside_homes_in("ui/menus.rs", around, HOMES);
    assert_eq!(found.len(), 3, "{found:?}");
    let wrong = outside_their_homes(HOMES);
    assert!(
        wrong.is_empty(),
        "a sidecar of the user's written around write_sidecars:\n{}",
        wrong.join("\n")
    );
}

/// A language alternative (the comment, the alt text, the extended description) is written
/// only by `xmp::set_alt_text` and `xmp::comment_element`. Each takes the field's other
/// languages from the packet its shown text was read from, which is the image when the sidecar
/// lacks the field. Before, the comment took them from the sidecar alone, so the first sidecar
/// of a photo whose comment had several languages kept only the one shown. A new writer of such
/// a field goes through one of these, or chooses its packet as they do.
#[test]
fn language_fields_keep_the_languages_read() {
    const HOMES: &[(&str, &[(&str, &str)])] = &[(
        "language_alternative(",
        &[
            ("metadata/xmp.rs", "set_alt_text"),
            ("metadata/xmp.rs", "comment_element"),
        ],
    )];
    let wrong = outside_their_homes(HOMES);
    assert!(
        wrong.is_empty(),
        "a language alternative written outside set_alt_text and comment_element:\n{}",
        wrong.join("\n")
    );
}

/// Only Glimt's own files are written with `file_io::replace_file` and the writers built on it
/// (`write_atomic`, `edit_shared`). They write through a link to the file it points to, as a
/// settings file kept in a repository is a link, and do not refuse a read-only file. A file of
/// the user's goes through `replace_name`, which replaces a link itself and refuses a read-only
/// file, or `replace_existing` for an image written again. Before, clippy's reasons sent every
/// write to `replace_file`, so a new writer of a user's file could follow a planted link or
/// write over a photo made read-only.
#[test]
fn only_glimts_own_files_are_written_through_links() {
    const HOMES: &[(&str, &[(&str, &str)])] = &[
        (
            "replace_file(",
            &[
                ("files/file_io.rs", "edit_shared"),
                ("files/file_io.rs", "write_atomic"),
                ("features/remote.rs", "mark_focused"),
            ],
        ),
        ("write_atomic(", &[("main.rs", "on_exit")]),
        (
            "edit_shared(",
            &[
                ("features/copy_for_sharing.rs", "save_choice"),
                ("files/config.rs", "write_settings"),
                ("metadata/marks.rs", "write"),
                ("model/folder.rs", "edit_recent"),
                ("ui/file_list.rs", "change_sort_folders"),
                ("ui/info_panel.rs", "change_keyword_tree"),
            ],
        ),
    ];
    let wrong = outside_their_homes(HOMES);
    assert!(
        wrong.is_empty(),
        "replace_file used for a file that may be the user's (use file_io::replace_name):\n{}",
        wrong.join("\n")
    );
}

/// The view's colour settings (Preferences' profiles, colour management on or off and the view
/// filter) reach only what is shown. Everything Glimt writes or hands to other programs (the
/// shared thumbnail cache, a print, the clipboard, an exported frame) goes through
/// `colour::to_srgb`, which reads none of them. Before, the settings changed the colours of the
/// shared cache and of prints.
#[test]
fn view_colours_stay_in_the_view() {
    // The view decodes and animates what is shown, and the thumbnail workers turn a cached
    // sRGB thumbnail into the screen's colours to show it. colour.rs keeps the settings.
    const SHOWN: &[(&str, &str)] = &[
        ("images/colour.rs", ""),
        ("images/decode.rs", "decode"),
        ("images/decode.rs", "animation"),
        ("ui/workers.rs", "spawn_thumbnailers"),
    ];
    let homes = [
        "to_display(",
        "COLOUR.read(",
        "COLOR_MANAGEMENT.load(",
        "VIEW_FILTER.load(",
    ]
    .map(|text| (text, SHOWN));
    let wrong = outside_their_homes(&homes);
    assert!(
        wrong.is_empty(),
        "the view's colour settings used outside the view (use colour::to_srgb):\n{}",
        wrong.join("\n")
    );
}

/// A dialog or window reads its Enter or Esc only through `App::answers`, which leaves the key to
/// the widget with the keyboard focus. Read from the input as a whole, the key also answered
/// while egui gave it to that widget: Shift+Delete, Tab to "Cancel" and Enter deleted the file
/// for good. Every way of reading a key names it, so the rule is on the name, not on the read:
/// `Key::Enter` and `Key::Escape` appear only as the key of an `answers(…)` call, and in the
/// homes below. A text field's own Enter goes through `dialogs::entered`, and the shortcuts'
/// table is read in `read_keys` only while `App::focus_held` says no widget has the key. A
/// `use` that renames `Key` or brings in its variants would hide the name, so it counts too.
#[test]
fn keys_answer_through_one_place() {
    const HOMES: &[(&str, &str)] = &[
        // The one place that reads a dialog's key, and the one for a text field's Enter.
        ("ui/dialogs.rs", "answers"),
        ("ui/dialogs.rs", "entered"),
        // Whether a widget has the keys, which looks back at the last frame's focus only for
        // the Enter or Esc that took it away. It reads no key for an answer.
        ("ui/dialogs.rs", "focus_held"),
        // The shortcuts' table, and the keyboard map in Help, which only draws the keys.
        ("ui/actions.rs", "SHORTCUTS"),
        ("ui/help.rs", "ROWS"),
        // Esc in Preferences' search field clears the search first, and closes it after.
        ("ui/preferences.rs", "preferences_window"),
        // A key being chosen for an action: Esc gives up the choice.
        ("ui/preferences.rs", "key_settings"),
        ("ui/preferences.rs", "pressed_key"),
        // The projector's own window has no widget that takes the focus.
        ("viewer/view.rs", "projector_window"),
    ];
    // Any key is read only in these functions, whatever key it is. A key read anywhere else,
    // such as by a new view's own arrows, would be taken while a dialog is open or a text field
    // has the focus, or after the shortcut that has it.
    const PRESSED: &[(&str, &str)] = &[
        // The keys read before the shortcuts: Enter in the grid, a zoomed image's arrows, the
        // keys that extend the selection with Shift (`set_extends_selection`), and ↑ and ↓ for
        // the icon grid's rows (`set_row_step`).
        ("ui/actions.rs", "read_keys"),
        // Tab, which only shows where the focus went, and takes no key from anyone.
        ("ui/actions.rs", "focus_ring"),
        ("ui/dialogs.rs", "entered"),
        ("ui/preferences.rs", "preferences_window"),
        ("viewer/view.rs", "projector_window"),
    ];
    const KEY_READS: &[(&str, &[(&str, &str)])] = &[
        ("key_pressed(", PRESSED),
        ("consume_key(", &[("ui/actions.rs", "read_keys")]),
        // The shortcuts' table, read in one place.
        ("consume_shortcut(", &[("ui/actions.rs", "shortcut")]),
        // The events themselves: a dialog's answer, the focus that Enter or Esc took away, and
        // a key being chosen in Preferences.
        (
            "Event::Key {",
            &[
                ("ui/dialogs.rs", "answers"),
                ("ui/dialogs.rs", "focus_held"),
                ("ui/preferences.rs", "pressed_key"),
            ],
        ),
        ("key_down(", &[]),
        // The set of keys held down, a field of the input that a key is read from too.
        ("keys_down", &[]),
        ("key_released(", &[]),
        ("num_presses(", &[]),
    ];
    let mut wrong = Vec::new();
    for (file, text) in rust_sources() {
        let lines = code_lines(&text);
        let end = tests_start(&lines);
        let mut item = String::new();
        for (i, line) in lines[..end].iter().enumerate() {
            // A top-level line starts a new item, which is no home unless it is named as one.
            if line.starts_with(|c: char| c.is_alphabetic()) {
                item.clear();
            }
            if let Some(name) = function_name(line).or_else(|| constant_name(line)) {
                item = name.to_owned();
            }
            let home = HOMES.iter().any(|(f, s)| file == *f && item == *s);
            if names_enter_or_esc(line) && !home {
                wrong.push(format!("{file}:{} in {item}", i + 1));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "Enter or Esc read from the whole input (use App::answers or dialogs::entered):\n{}",
        wrong.join("\n")
    );
    // The rule sees the key however it is read, also away from the read and beside a field's
    // own Enter, and lets only the key of an answer through.
    for caught in [
        "let k = egui::Key::Enter;",
        "x.lost_focus() || i.key_pressed(Key::Enter)",
        "i.num_presses(egui::Key::Escape) > 0",
        "i.consume_shortcut(&KeyboardShortcut::new(NONE, Key::Escape))",
        "self.answers(ctx, egui::Key::Enter) || i.key_released(Key::Enter)",
        "use egui::Key as K;",
        "use egui::Key::*;",
        "type K = egui::Key;",
        "pub(crate) type K = Key;",
        "if i.key_pressed(egui::Key::from_name(\"      \").unwrap()) {",
    ] {
        assert!(names_enter_or_esc(caught), "missed: {caught}");
    }
    for answer in [
        "if self.answers(ctx, egui::Key::Escape) {",
        "&& self.answers(ui.ctx(), Key::Enter)",
        "let key = egui::Key::from_name(parts.pop()?)?;",
        "type Keys = Vec<Key>;",
    ] {
        assert!(!names_enter_or_esc(answer), "caught an answer: {answer}");
    }
    let wrong = outside_their_homes(KEY_READS);
    assert!(
        wrong.is_empty(),
        "a key read outside its homes (read it in App::read_keys, before the shortcut that \
         would take it, and only while no dialog is open and no widget has the focus):\n{}",
        wrong.join("\n")
    );
    let planted = "fn grid() {\n    if i.key_down(egui::Key::A) {}\n}\n";
    assert_eq!(
        outside_homes_in("ui/file_list.rs", planted, KEY_READS),
        ["ui/file_list.rs:2 (key_down()"]
    );
    let planted = "fn grid() {\n    if i.keys_down.contains(&egui::Key::A) {}\n}\n";
    assert_eq!(
        outside_homes_in("ui/file_list.rs", planted, KEY_READS),
        ["ui/file_list.rs:2 (keys_down)"]
    );
}

/// Every modal dialog is in `App::modal_open`, the one list of what is open. The shortcuts in
/// `read_keys` wait while it says a dialog is open, and Esc goes to the dialog first. A modal
/// left out of it would let a key under it run an action on the files while the dialog asks
/// about them. A modal is named with `Id::unique("…")`, and its name, with `_` for a space or a
/// dash, is the last name of a term of the list, such as "confirm-delete" for
/// `self.dialogs.confirm_delete.is_some()`. Before, a part of a name counted, so a new modal
/// named "confirm" passed on `confirm_delete`, and then any word did, so "folder" passed on
/// `self.folder.asks_login()`.
#[test]
fn every_modal_is_in_modal_open() {
    let dialogs = code_lines(&source("ui/dialogs.rs"));
    let start = (dialogs.iter())
        .position(|l| function_name(l) == Some("modal_open"))
        .unwrap();
    let list = dialogs[start..=block_end(&dialogs, start).unwrap()].join("\n");
    let mut wrong = Vec::new();
    for (file, text) in rust_sources() {
        wrong.extend(modals_outside_in(&file, &text, &list));
    }
    assert!(
        wrong.is_empty(),
        "a modal not in App::modal_open, or not named with egui::Id::unique(\"…\"):\n{}",
        wrong.join("\n")
    );
    let list = "self.dialogs.confirm_delete.is_some() || self.folder.asks_login()";
    let planted = "fn ask() {\n    egui::Modal::new(egui::Id::unique(\"confirm\")).show();\n    \
        egui::Modal::new(\n        egui::Id::unique(\"login\"),\n    );\n    \
        Modal::new(Id::unique(\"confirm-delete\"));\n    Modal::new(\n        \
        egui::Id::unique(\n            \"asks login\",\n        ),\n    );\n    \
        Modal::new(id);\n    Modal::new(Id::unique(\"folder\"));\n}\n";
    assert_eq!(
        modals_outside_in("a.rs", planted, list),
        [
            "a.rs:2 (confirm)",
            "a.rs:3 (login)",
            "a.rs:12 ()",
            "a.rs:13 (folder)"
        ]
    );
}

/// Returns each modal in `text`, the source of `file`, whose name is not the last name of a
/// `||` term of `list`, the code of `App::modal_open`, as "file:line (name)". A modal that is
/// not named with `Id::unique("…")` is returned with an empty name. The call is read with its
/// lines joined, so a call that rustfmt splits over several lines is still seen.
fn modals_outside_in(file: &str, text: &str, list: &str) -> Vec<String> {
    // Only the last name counts, such as `asks_login` in `self.folder.asks_login()`. A word
    // before it, such as `folder` or `dialogs`, names what holds the dialog, not a dialog.
    let names: Vec<&str> = (list.split("||"))
        .map(|term| {
            let term = term.trim_end_matches(|c: char| c == '}' || c.is_whitespace());
            let term = term
                .trim_end_matches(".is_some()")
                .trim_end_matches(".is_empty()");
            let term = term.trim_end_matches("()");
            term.rsplit('.').next().unwrap_or_default()
        })
        .collect();
    let lines = code_lines(text);
    let raw: Vec<&str> = text.lines().collect();
    let end = tests_start(&lines);
    let mut wrong = Vec::new();
    for (i, line) in lines[..end].iter().enumerate() {
        // The code line says it is a call, not a word in a string, and the raw line has the name.
        let Some(at) = (line.contains("Modal::new("))
            .then(|| raw[i].find("Modal::new("))
            .flatten()
        else {
            continue;
        };
        // The call with its next lines joined, each without its indent.
        let call: String = (raw[i][at + "Modal::new(".len()..].chars())
            .chain(
                raw[i + 1..(i + 4).min(raw.len())]
                    .iter()
                    .flat_map(|l| l.trim().chars()),
            )
            .collect();
        let name = (call.strip_prefix("egui::").unwrap_or(&call))
            .strip_prefix("Id::unique(\"")
            .and_then(|rest| rest.split_once('"'))
            .map(|(name, _)| name);
        let whole = name.is_some_and(|n| names.contains(&n.replace([' ', '-'], "_").as_str()));
        if !whole {
            wrong.push(format!("{file}:{} ({})", i + 1, name.unwrap_or("")));
        }
    }
    wrong
}

/// Every module lives in the folder of what it does, so `src/` itself holds only main.rs, cli.rs
/// and one file for each folder. A folder's file holds its `//!` doc and its `mod` lines and
/// nothing else. A `pub use` there would let other files import a name through the folder, which
/// hides the module that defines it.
#[test]
fn modules_live_in_their_folder() {
    const ROOT: &[&str] = &["main.rs", "cli.rs"];
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut wrong = Vec::new();
    for (path, text) in rust_sources() {
        if ROOT.contains(&path.as_str()) {
            continue;
        }
        // A folder's file is the one beside a folder of its name, at any depth.
        if !src.join(path.trim_end_matches(".rs")).is_dir() {
            if !path.contains('/') {
                wrong.push(format!("{path}: put it in the folder of what it does"));
            }
            continue;
        }
        let lines = code_lines(&text);
        for (i, line) in lines.iter().enumerate() {
            let name = without_qualifiers(line)
                .strip_prefix("mod ")
                .and_then(|l| l.strip_suffix(';'));
            let is_mod = name.is_some_and(|n| n.chars().all(|c| c == '_' || c.is_alphanumeric()));
            let is_cfg = line.starts_with("#[cfg(") && line.ends_with(")]") && !line.contains("] ");
            if !line.is_empty() && !is_mod && !is_cfg {
                wrong.push(format!(
                    "{path}:{}: a folder's file holds only mod lines",
                    i + 1
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A test that feeds `test_support::broken` is named `survives_broken_*`. The weekly run with
/// five million broken inputs (`.github/workflows/parsers.yml`) picks its tests by that name,
/// and it would skip one named otherwise. Every spelling of the call counts, imported or by its
/// whole path.
#[test]
fn broken_inputs_are_fed_by_survives_broken_tests() {
    let mut wrong = Vec::new();
    for (file, text) in rust_sources() {
        if file.ends_with("test_support.rs") {
            continue;
        }
        let mut function = String::new();
        for (i, line) in code_lines(&text).iter().enumerate() {
            if let Some(name) = function_name(line) {
                function = name.to_owned();
            }
            let calls = line.match_indices("broken(").any(|(at, _)| {
                let before = line[..at].chars().next_back();
                !before.is_some_and(|c| c.is_alphanumeric() || c == '_')
            });
            if calls && !function.starts_with("survives_broken") {
                wrong.push(format!("{file}:{} in {function}", i + 1));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "test_support::broken fed outside a survives_broken_* test:\n{}",
        wrong.join("\n")
    );
}

/// What Glimt reads from outside its own code is read by a parser that a `survives_broken_*`
/// test feeds broken input: a file read whole, a program's output and a line from the socket
/// (`OUTSIDE_TEXT_READS`). So each file that reads such text has a `survives_broken_*` test of
/// its own, or the read says which test feeds its parser elsewhere, by that test's name in a
/// comment on it or just above it. A `.trashinfo` on a stick and a command on the socket were
/// read with no such test.
#[test]
fn outside_text_is_read_by_parsers_fed_broken_input() {
    let sources = rust_sources();
    let fuzzing_tests: HashSet<String> = (sources.iter())
        .flat_map(|(_, text)| code_lines(text))
        .filter_map(|line| function_name(&line).map(str::to_owned))
        .filter(|name| name.starts_with("survives_broken_"))
        .collect();
    let mut wrong = Vec::new();
    for (file, text) in &sources {
        let lines = reads_without_fuzzing_test(text, &fuzzing_tests);
        wrong.extend(lines.iter().map(|line| format!("{file}:{line}")));
    }
    assert!(
        wrong.is_empty(),
        "outside text read with no survives_broken_* test of its parser (add one, or name the \
         test that feeds the parser in a comment on the read):\n{}",
        wrong.join("\n")
    );
}

/// Returns the lines of `text` outside the tests that read outside text (`OUTSIDE_TEXT_READS`)
/// when `text` has no `survives_broken_*` test, unless a comment on the read's statement or
/// just above it names one of `fuzzing_tests`.
fn reads_without_fuzzing_test(text: &str, fuzzing_tests: &HashSet<String>) -> Vec<usize> {
    let (code, comments) = (code_lines(text), comment_lines(text));
    let end = tests_start(&code);
    let has_its_own = (code[end..].iter())
        .any(|l| function_name(l).is_some_and(|n| n.starts_with("survives_broken_")));
    if has_its_own {
        return Vec::new();
    }
    let names_a_test = |comment: &str| {
        (comment.match_indices("survives_broken_")).any(|(at, _)| {
            let name: String = (comment[at..].chars())
                .take_while(|&c| c.is_alphanumeric() || c == '_')
                .collect();
            fuzzing_tests.contains(&name)
        })
    };
    let mut found = Vec::new();
    for i in 0..end {
        if !OUTSIDE_TEXT_READS
            .iter()
            .any(|r| mentions_call(&code[i], r))
        {
            continue;
        }
        // Up to the statement's first line, then up the comment lines just above it.
        let mut first = i;
        while first > 0
            && !code[first - 1].trim().is_empty()
            && !code[first - 1].ends_with([';', '{', '}'])
        {
            first -= 1;
        }
        while first > 0
            && code[first - 1].trim().is_empty()
            && !comments[first - 1].trim().is_empty()
        {
            first -= 1;
        }
        if !(first..=i).any(|j| names_a_test(&comments[j])) {
            found.push(i + 1);
        }
    }
    found
}

/// The check above finds a read in a file without a `survives_broken_*` test, also one whose
/// comment names a test that is not there. It passes a read whose comment, on its statement
/// or above it, names a test there, and every read of a file with a test of its own.
#[test]
fn finds_outside_text_read_without_its_fuzzing_test() {
    let tests = HashSet::from(["survives_broken_files".to_owned()]);
    let text = "fn a() {\n    let t = read_text(p);\n    // Fed by survives_broken_gone.\n    \
        let u = read_limited(p);\n    // Fed by survives_broken_files.\n    let v = x\n        \
        .and_then(|p| read_text(p));\n    let w = short_output_of(\"lpstat\"); // survives_broken_files\n}\n";
    assert_eq!(reads_without_fuzzing_test(text, &tests), [2, 4]);
    let with_its_own =
        format!("{text}#[cfg(test)]\nmod tests {{\n    fn survives_broken_a() {{}}\n}}\n");
    assert!(reads_without_fuzzing_test(&with_its_own, &tests).is_empty());
}

/// A comment fits the 100 columns rustfmt keeps code to, which rustfmt does not wrap: a longer
/// one is read sideways, or wrapped by an editor in the middle of a word. Also one at the end
/// of a line of code.
#[test]
fn comments_fit_the_width() {
    let mut wrong = Vec::new();
    for (file, text) in rust_sources() {
        for (i, (line, code)) in text.lines().zip(code_lines(&text)).enumerate() {
            // Where the code ends, the rest of the line is a comment, not a string going on.
            let rest: String = line.chars().skip(code.chars().count()).collect();
            if line.chars().count() > 100 && rest.trim_start().starts_with("//") {
                wrong.push(format!("{file}:{}", i + 1));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "comments over 100 columns:\n{}",
        wrong.join("\n")
    );
}

/// The .deb and the .rpm install the same files, and the AppImage, the tar.gz and the Flatpak
/// each of them that is not documentation: five lists kept in step by this test, not by memory.
#[test]
fn packages_install_the_same_files() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let read = |name: &str| std::fs::read_to_string(root.join(name)).unwrap();
    let cargo = read("Cargo.toml");
    let deb = cargo.split_once("[package.metadata.deb]").unwrap().1;
    let (deb, rpm) = deb.split_once("[package.metadata.generate-rpm]").unwrap();
    fn quoted<'a>(line: &'a str, start: &str) -> Option<&'a str> {
        line.split_once(start)?.1.split('"').next()
    }
    // The .deb's license-file is its LICENSE.
    let mut deb: Vec<&str> = deb.lines().filter_map(|l| quoted(l, "[\"")).collect();
    let mut rpm: Vec<&str> = rpm
        .lines()
        .filter_map(|l| quoted(l, "source = \""))
        .collect();
    deb.sort();
    rpm.sort();
    assert_eq!(
        deb, rpm,
        "Cargo.toml: the .deb's and the .rpm's assets differ"
    );
    let release = read(".github/workflows/release.yml");
    let step = |name: &str| {
        let at = release.find(&format!("- name: {name}\n")).unwrap();
        release[at..]
            .split("\n      - name: ")
            .next()
            .unwrap()
            .to_owned()
    };
    let others = [
        ("the tar.gz", step("tar.gz")),
        ("the AppImage", step("AppImage")),
        ("the Flatpak", read("flatpak/io.github.thornbux.Glimt.yml")),
    ];
    let docs = ["README.md", "CHANGELOG.md", "examples/"];
    let mut wrong = Vec::new();
    for file in rpm
        .iter()
        .filter(|f| !docs.iter().any(|d| f.starts_with(d)))
    {
        // The binary is built to target/<the target>/release in the release workflow.
        let name = file.trim_start_matches("target/");
        for (package, text) in &others {
            if !text.contains(name) {
                wrong.push(format!("{package}: {file}"));
            }
        }
    }
    assert!(wrong.is_empty(), "not installed:\n{}", wrong.join("\n"));
}

/// Every optional feature is in `full`, which the packages are built with, and has a row in
/// README's table. CI builds with --all-features, so a feature left out of `full` would pass
/// every check and still be missing from every package.
#[test]
fn full_has_every_feature() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let read = |name: &str| std::fs::read_to_string(root.join(name)).unwrap();
    let cargo = read("Cargo.toml");
    let readme = read("README.md");
    let features = cargo.split_once("\n[features]\n").unwrap().1;
    let features = features.split("\n[").next().unwrap();
    let names = features
        .lines()
        .filter_map(|l| Some(l.split_once(" = ")?.0));
    let full = features.lines().find(|l| l.starts_with("full = ")).unwrap();
    let mut wrong = Vec::new();
    for name in names.filter(|n| !["full", "test-hooks"].contains(n)) {
        if !full.contains(&format!("\"{name}\"")) {
            wrong.push(format!("Cargo.toml: {name} is not in full"));
        }
        if !readme.contains(&format!("\n| `{name}` |")) {
            wrong.push(format!(
                "README.md: {name} has no row in the features table"
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// The file list is drawn in two places at once, the side panel and the grid in the image area,
/// each in its own form. The functions that draw it take that form from `file_list`'s
/// parameters, so a setting read there would draw the side panel as the grid, or the other way.
/// The settings are read only where the side panel picks its form and where they are set, so a
/// helper the drawing calls cannot read them either, whatever the settings are called through.
#[test]
fn the_list_is_drawn_in_the_form_its_caller_asks() {
    const HOMES: &[&str] = &["side_panel", "set_thumbnails"];
    let lines = code_lines(&source("ui/file_list.rs"));
    let end = tests_start(&lines);
    let mut function = String::new();
    let mut wrong = Vec::new();
    for (i, line) in lines[..end].iter().enumerate() {
        if let Some(name) = function_name(line) {
            function = name.to_owned();
        }
        let setting = [".thumbnails", ".icon_grid"].iter().any(|s| {
            (line.match_indices(s)).any(|(at, _)| {
                let after = line[at + s.len()..].chars().next();
                !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
            })
        });
        if setting && !HOMES.contains(&function.as_str()) {
            wrong.push(format!("ui/file_list.rs:{} in {function}", i + 1));
        }
    }
    assert!(
        wrong.is_empty(),
        "the list's form read from the settings, not from file_list's parameters:\n{}",
        wrong.join("\n")
    );
}

/// The file list is drawn only by the side panel and the grid in the image area. A new view of
/// the list needs what each of them has, which the grid of 0.65.0 missed through five audit
/// rounds, so a third caller of `App::file_list` fails here until it is added with them. The
/// functions that `file_list` draws a row or a cell with are watched too, because a new view
/// could call them directly. Only the Pan view does, for the cells of a day's images and their
/// clicks, which select as the list's own do.
#[test]
fn the_list_is_drawn_by_known_views() {
    const VIEWS: &[(&str, &[(&str, &str)])] = &[
        (
            "file_list(",
            &[
                ("ui/file_list.rs", "side_panel"),
                ("ui/file_list.rs", "area_grid"),
                // The clipboard's own `file_list`, which is arboard's and draws nothing.
                ("ui/actions.rs", "copy_files"),
            ],
        ),
        ("file_row(", &[("ui/file_list.rs", "file_list")]),
        (
            "file_cell(",
            &[
                ("ui/file_list.rs", "file_list"),
                ("viewer/pan.rs", "pan_timeline"),
            ],
        ),
        (
            "row_clicked(",
            &[
                ("ui/file_list.rs", "file_list"),
                ("viewer/pan.rs", "pan_timeline"),
            ],
        ),
    ];
    let wrong = outside_their_homes(VIEWS);
    assert!(
        wrong.is_empty(),
        "a new view of the list: give it everything in AGENTS.md's \"A new way to show the \
         list\", then add it to the views here:\n{}",
        wrong.join("\n")
    );
    let planted = "fn pan() {\n    let opened = self.file_list(ctx, ui, &mut a, &mut b, f);\n}\n";
    assert_eq!(
        outside_homes_in("viewer/pan.rs", planted, VIEWS),
        ["viewer/pan.rs:2 (file_list()"]
    );
    let planted = "fn strip() {\n    let row = self.file_row(ui, i, h, m, t);\n    \
        self.row_clicked(ctx, &row, i);\n    self.file_cell(ui, i, r, m);\n}\n";
    assert_eq!(
        outside_homes_in("viewer/view.rs", planted, VIEWS),
        [
            "viewer/view.rs:2 (file_row()",
            "viewer/view.rs:3 (row_clicked()",
            "viewer/view.rs:4 (file_cell()"
        ]
    );
}

/// Every widget drawn by hand that takes a click tells a screen reader what it is: a row or
/// cell of the list through `speak_item`, the image through `speak_image`, and the others with
/// `widget_info`. egui's own widgets do this themselves, but a widget drawn by hand says
/// nothing, and Orca was silent in the grid in the image area. The widget's response is bound
/// with `let`, and a later line of the same function speaks about that name, so another
/// widget's `widget_info` close by does not count, and one far below does.
#[test]
fn every_clickable_widget_drawn_by_hand_speaks() {
    let names = |line: &str, name: &str| {
        (line.match_indices(name)).any(|(at, _)| {
            let word = |c: char| c.is_alphanumeric() || c == '_';
            !line[..at].chars().next_back().is_some_and(word)
                && !line[at + name.len()..].chars().next().is_some_and(word)
        })
    };
    let mut wrong = Vec::new();
    for (file, text) in rust_sources() {
        let lines = code_lines(&text);
        let end = tests_start(&lines);
        for (i, line) in lines[..end].iter().enumerate() {
            if !takes_a_click(line) {
                continue;
            }
            // The statement's first line, as rustfmt may break a long one, and the last name
            // its `let` binds: `response` in `let (rect, response) = …`. The line before it
            // ends another statement or block, or is blank (a comment, as code_lines blanks
            // comments), or is an attribute.
            let ends =
                |l: &str| l.trim().is_empty() || l.trim_end().ends_with([';', '{', '}', ']']);
            let start = ((0..=i).rev())
                .take_while(|&k| k == i || !ends(&lines[k]))
                .last()
                .unwrap_or(i);
            let bound = (lines[start].trim_start().strip_prefix("let "))
                .and_then(|l| l.split(['=', ':']).next())
                .and_then(|l| {
                    l.trim()
                        .trim_end_matches(')')
                        .rsplit([',', '(', ' '])
                        .next()
                })
                .filter(|name| !name.is_empty());
            let speaks = bound.is_some_and(|name| {
                (lines[i + 1..end].iter())
                    .take_while(|l| function_name(l).is_none())
                    .any(|l| (l.contains("speak_") || l.contains("widget_info")) && names(l, name))
            });
            if !speaks {
                wrong.push(format!("{file}:{}", i + 1));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "a widget that takes a click with nothing for a screen reader:\n{}",
        wrong.join("\n")
    );
}

/// Every release in CHANGELOG.md has its compare link at the foot, and [Unreleased] compares
/// from the newest release, so each heading is a link. 0.70.0's version commit left both out.
#[test]
fn every_release_in_the_changelog_has_its_link() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let log = std::fs::read_to_string(root.join("CHANGELOG.md")).unwrap();
    let versions: Vec<&str> = (log.lines())
        .filter_map(|l| l.strip_prefix("## [")?.split(']').next())
        .filter(|v| *v != "Unreleased")
        .collect();
    let missing: Vec<&&str> = (versions.iter())
        .filter(|v| !log.contains(&format!("\n[{v}]: https://github.com/thornbux/glimt/")))
        .collect();
    assert!(missing.is_empty(), "no link at the foot for {missing:?}");
    let newest = versions.first().unwrap();
    let unreleased =
        format!("[Unreleased]: https://github.com/thornbux/glimt/compare/v{newest}...HEAD");
    assert!(
        log.contains(&unreleased),
        "[Unreleased] does not compare from v{newest}"
    );
}

/// Every operation that leaves an Undo takes `undo_on_the_way` when it starts, and its thread
/// holds it until the report is sent, so Ctrl+Z waits for it instead of undoing the operation
/// before it. An operation leaves an Undo when it sends an `OperationReport` with an `undo`, or
/// runs `rename_undoable`. What an Undo from the trash could not put back
/// (`is_undo_remainder`) is not counted, because it never replaces a newer Undo.
#[test]
fn operations_that_leave_an_undo_make_undo_wait() {
    let mut missing = Vec::new();
    for (file, text) in rust_sources() {
        missing.extend(undo_left_without_waiting(&file, &text));
    }
    assert!(
        missing.is_empty(),
        "an operation leaves an Undo without undo_on_the_way():\n{}",
        missing.join("\n")
    );
}

/// Returns the functions of `text`, the source of `file`, that leave an Undo outside the tests
/// without taking `undo_on_the_way`, as "file: function".
fn undo_left_without_waiting(file: &str, text: &str) -> Vec<String> {
    let lines = code_lines(text);
    let end = tests_start(&lines);
    let mut missing = Vec::new();
    let mut function = String::new();
    let (mut sends_report, mut leaves_undo, mut waits) = (false, false, false);
    let mut is_remainder = false;
    for line in lines[..end].iter().map(String::as_str).chain(["fn end"]) {
        if let Some(name) = function_name(line) {
            if leaves_undo && !waits && !is_remainder {
                missing.push(format!("{file}: {function}"));
            }
            function = name.to_owned();
            (sends_report, leaves_undo, waits, is_remainder) = (false, false, false, false);
            continue;
        }
        is_remainder |= line.contains("is_undo_remainder: true");
        let field = line.trim_start();
        sends_report |= line.contains(".send(OperationReport {");
        leaves_undo |= sends_report && (field == "undo," || field.starts_with("undo:"));
        leaves_undo |= line.contains("rename_undoable(");
        waits |= line.contains("undo_on_the_way()");
    }
    missing
}

/// The check above finds a report with an Undo and a rename that leaves one. It passes a
/// function that takes the token, one that only reads a report, and an Undo's remainder.
#[test]
fn finds_an_undo_left_without_waiting() {
    let text = "fn a() {\n    tx.send(OperationReport {\n        undo,\n    });\n}\n\
        fn b() {\n    let t = self.undo_on_the_way();\n    tx.send(OperationReport {\n        \
        undo: None,\n    });\n}\nfn c() {\n    tx.send(rename_undoable(f, t));\n}\n\
        fn d() {\n    for OperationReport {\n        undo,\n    } in x {}\n}\n\
        fn e() {\n    tx.send(OperationReport {\n        undo,\n        is_undo_remainder: true,\n\
        });\n}\n";
    assert_eq!(
        undo_left_without_waiting("x.rs", text),
        ["x.rs: a", "x.rs: c"]
    );
}

/// A rename that later steps count on is on disk with its folder before they run: the function
/// that renames syncs the folder (`file_io::sync_folder_of` or `sync_folder`), or hands the
/// rename to `write_beside`, which does. Files were synced but their folders never were, so
/// after a power cut a move to a USB stick could lose both the source and its copy, and an older
/// sidecar come back. The renames listed here stand alone: one rename is one step, so a power
/// cut leaves the old name or the new one, and nothing after it counts on the new one. A new
/// rename is synced or added here with its reason. A move through `put_file` counts as a
/// rename here, because its move on one disk is a rename that it does not sync: its caller does,
/// once for the folder of all its moves. A copy (`put_file(…, true)`) is not a rename. Copies
/// go through `put_file`, which syncs before a move to another disk removes the source, and
/// never through `std::fs::copy`. Clippy refuses that too, but an allow would let it pass there.
#[test]
fn renames_that_later_steps_count_on_are_synced() {
    const STANDING_ALONE: &[(&str, &str)] = &[
        ("files/file_io.rs", "rename_never_over"), // the rename itself, synced by its callers
        ("files/file_io.rs", "put_file"),          // its move on one disk, synced by its callers
        ("files/journal.rs", "rename_step"),       // a step, synced by `sync_folders` after
        ("files/trash.rs", "trash_by_hand"),       // a file into the trash on its own disk
        ("images/thumbs.rs", "write_thumbnail"),   // a thumbnail, made again when it is lost
        ("metadata/xmp.rs", "follow"),             // a sidecar after its file
        ("model/collections.rs", "rename"),        // a collection renamed
    ];
    let mut wrong = Vec::new();
    let mut renaming = Vec::new();
    for (file, text) in rust_sources() {
        for (function, synced) in renames_in(&text) {
            let listed = STANDING_ALONE.contains(&(file.as_str(), function.as_str()));
            if !synced && !listed {
                wrong.push(format!("{file}: {function}"));
            }
            renaming.push((file.clone(), function));
        }
    }
    for (file, function) in STANDING_ALONE {
        if !renaming.iter().any(|(f, s)| f == file && s == function) {
            wrong.push(format!(
                "{file}: {function} (listed, but renames nothing now)"
            ));
        }
    }
    wrong.extend(outside_their_homes(&[("fs::copy(", &[])]));
    assert!(
        wrong.is_empty(),
        "a rename without its folder synced (file_io::sync_folder_of), or a copy outside \
         put_file:\n{}",
        wrong.join("\n")
    );
}

/// A name is never removed on the belief that another name still holds the file, when that
/// belief rests on a look a share may answer from its cache. A share that did a step but lost
/// its answer still shows the old name, and removing the other name then removes the only one
/// left. So a file is linked under a second name only by `rename_never_over`, whose
/// `unlink_linked` takes the new name back only when the unlink surely removed nothing
/// (`unlink_was_refused`). And a second name of a file is removed only after
/// `same_file_by_opening`, never after `same_file`, a look. Glimt's own socket is linked too,
/// by `link_socket`, and is no user's file.
#[test]
fn no_name_is_removed_on_a_cached_look() {
    let mut wrong = outside_their_homes(&[(
        "hard_link(",
        &[
            ("files/file_io.rs", "rename_never_over"),
            ("features/remote.rs", "link_socket"),
        ],
    )]);
    for (file, text) in rust_sources() {
        let found = removals_after_a_look(&text);
        wrong.extend(
            found
                .into_iter()
                .map(|f| format!("{file}: {f} (same_file)")),
        );
    }
    assert!(
        wrong.is_empty(),
        "a file linked outside rename_never_over, or a name removed after same_file rather \
         than same_file_by_opening:\n{}",
        wrong.join("\n")
    );
}

/// A path through an opened folder (`file_io::path_through`, `/proc/self/fd/N/…`) is used only
/// in the modules that show every path through `file_io::shown_path`, which names it by the
/// folder's own path. The trash at the top of a disk is reached through such a path, and the
/// text of an unlink that may have been done named `/proc/self/fd/12/files/a.jpg`, which the
/// user cannot open, in the one message whose job is to say where the file is. Questions about
/// the file system such a path is on are answered by the folder's own path in
/// `file_io::mount_holding`, which `a_path_through_an_opened_folder_is_asked_and_shown_by_its_name`
/// holds.
#[test]
fn paths_through_a_descriptor_are_shown_by_their_names() {
    const SHOWN_BY_NAME: [&str; 2] = ["files/file_io.rs", "files/trash.rs"];
    let homes: Vec<(&str, &str)> = SHOWN_BY_NAME.iter().map(|f| (*f, "")).collect();
    let mut wrong = outside_their_homes(&[("path_through(", &homes)]);
    for (file, text) in rust_sources() {
        if SHOWN_BY_NAME.contains(&file.as_str()) {
            wrong.extend(paths_made_into_text(&file, &text));
        }
    }
    assert!(
        wrong.is_empty(),
        "a path through a descriptor used outside the modules that show paths by their names, \
         or a path shown there without file_io::shown_path:\n{}",
        wrong.join("\n")
    );
}

/// Returns the places in `text`, of `file`, where a whole path is made into text outside
/// `shown_path` and the tests: through `.display()`, `.to_string_lossy()`, `.to_str()` or
/// `.into_string()`, or as Debug (`:?}`) in a format string. A path has no Display, so `{}`
/// takes one only through these, and an `OsStr` from `as_os_str()` becomes text only through
/// them too. The text of a last name alone (`file_name()`, `file_stem()`, `extension()`) is
/// not counted, as no path through a descriptor shows in it. A chain of calls over several
/// lines is read as one.
fn paths_made_into_text(file: &str, text: &str) -> Vec<String> {
    const TO_TEXT: [&str; 4] = [
        ".display()",
        ".to_string_lossy()",
        ".to_str()",
        ".into_string()",
    ];
    let code = code_lines(text);
    let comments = comment_lines(text);
    let end = tests_start(&code);
    let is_last_name = |receiver: &str| {
        let mut rest = receiver;
        while let Some(shorter) = (rest.strip_suffix('?'))
            .or_else(|| rest.strip_suffix(".unwrap_or_default()"))
            .or_else(|| rest.strip_suffix(".unwrap()"))
        {
            rest = shorter;
        }
        ["file_name()", "file_stem()", "extension()"]
            .iter()
            .any(|name| rest.ends_with(name))
    };
    let mut wrong = Vec::new();
    let mut function = String::new();
    for (i, (line, raw)) in code[..end].iter().zip(text.lines()).enumerate() {
        if line.starts_with(|c: char| c.is_alphabetic()) {
            function.clear();
        }
        if let Some(name) = function_name(line) {
            function = name.to_owned();
        }
        if function == "shown_path" {
            continue;
        }
        // The chain of calls that this line goes on with, from the line where it starts.
        let first = (0..=i)
            .rev()
            .find(|&j| !code[j].trim_start().starts_with('.'));
        let chain: String = code[first.unwrap_or(0)..=i]
            .iter()
            .map(|l| l.trim())
            .collect();
        let this_line = line.trim();
        let before_this_line = chain.len() - this_line.len();
        for call in TO_TEXT {
            for (at, _) in this_line.match_indices(call) {
                if !is_last_name(&chain[..before_this_line + at]) {
                    wrong.push(format!("{file}:{} ({call})", i + 1));
                }
            }
        }
        // The text of the line's literals, with its code and its comments blanked.
        let comment = comments.get(i).map_or("", String::as_str);
        let blank = std::iter::repeat(' ');
        let literals: String = (raw.chars())
            .zip(line.chars().chain(blank.clone()))
            .zip(comment.chars().chain(blank))
            .map(|((r, c), m)| if c == ' ' && m == ' ' { r } else { ' ' })
            .collect();
        if literals.contains(":?}") || literals.contains(":#?}") {
            wrong.push(format!("{file}:{} (:?}})", i + 1));
        }
    }
    wrong
}

/// The rule of `paths_through_a_descriptor_are_shown_by_their_names` finds a whole path made
/// into text in each spelling outside `shown_path`. It finds none inside `shown_path`, in the
/// tests, in a comment, or of a last name alone, also through a chain of calls over several
/// lines.
#[test]
fn finds_a_path_shown_without_its_name() {
    let text = "fn shown_path(path: &Path) -> String {\n    path.display().to_string()\n}\n\
        fn told(path: &Path) -> String {\n    format!(\"{}\", path.display())\n}\n\
        fn debug(path: &Path) -> String {\n    format!(\"{path:?}\")\n}\n\
        fn pretty(path: &Path) -> String {\n    format!(\"{:#?}\", path)\n}\n\
        fn lossy(path: &Path) -> String {\n    path.to_string_lossy().into()\n}\n\
        fn checked(path: &Path) -> Option<&str> {\n    path.to_str()\n}\n\
        fn bytes(path: &Path) -> String {\n    path.as_os_str().to_string_lossy().into()\n}\n\
        fn owned(path: PathBuf) -> String {\n    path.into_os_string().into_string().unwrap()\n}\n\
        fn name(path: &Path) -> String {\n    path.file_name()?.to_string_lossy().into()\n}\n\
        fn chained(path: &Path) -> String {\n    path.file_name()\n    .unwrap_or_default()\n\
        .to_string_lossy()\n    .into_owned()\n}\n\
        // Debug ({:?}) of a path is no text for the user.\n\
        mod tests {\n    fn t() { p.display(); format!(\"{p:?}\"); }\n}\n";
    assert_eq!(
        paths_made_into_text("files/trash.rs", text),
        [
            "files/trash.rs:5 (.display())",
            "files/trash.rs:8 (:?})",
            "files/trash.rs:11 (:?})",
            "files/trash.rs:14 (.to_string_lossy())",
            "files/trash.rs:17 (.to_str())",
            "files/trash.rs:20 (.to_string_lossy())",
            "files/trash.rs:23 (.into_string())",
        ]
    );
}

/// Returns the functions of `text` outside the tests that both look with `same_file` and
/// remove a name.
fn removals_after_a_look(text: &str) -> Vec<String> {
    let lines = code_lines(text);
    let end = tests_start(&lines);
    let mut found = Vec::new();
    let mut function = String::new();
    let (mut looks, mut removes) = (false, false);
    for line in lines[..end].iter().map(String::as_str).chain(["fn end"]) {
        if let Some(name) = function_name(line) {
            if looks && removes {
                found.push(function);
            }
            function = name.to_owned();
            (looks, removes) = (false, false);
            continue;
        }
        looks |= mentions(line, "same_file(");
        removes |= line.contains("remove_file(");
    }
    found
}

/// The check above finds a name removed after `same_file`, and not one removed after
/// `same_file_by_opening`, nor a look that removes nothing.
#[test]
fn finds_a_name_removed_after_a_cached_look() {
    let text = "fn a() {\n    if same_file(x, y) {\n        std::fs::remove_file(x)\n    }\n}\n\
        fn b() {\n    if same_file_by_opening(x, y) {\n        std::fs::remove_file(x)\n    }\n}\n\
        fn c() {\n    same_file(x, y)\n}\n";
    assert_eq!(removals_after_a_look(text), ["a"]);
}

/// A delete, a move to the trash, a move, a rename or a replace of a user's file looks first
/// whether the file is still the one the user picked or Glimt read. The function that makes the
/// call checks it itself (`still_the_one_picked`, `is_still_the_file`, `same_fingerprint`,
/// `replace_existing`, or `same_file_by_opening` for a second name of one file), and files
/// picked before a wait (a question, a prompt, the file chooser) come as `file_io::PickedFiles`
/// for it. Before, a delete, a trash or a rename confirmed in a dialog acted on whatever had the
/// name when the dialog closed, which another window or a sync may have given to another photo.
/// A sidecar is found at the act (`sidecars_moving_with`, `sidecars_of`), not picked before it,
/// so an act on a sidecar (on a line that names one, `side`) needs no check of its own in a
/// function that finds them. A finder there excuses no act on the photo itself.
/// What is no user's file is out of the rule by what it is: Glimt's own files are removed through
/// `file_io::remove_own`, or by a raw removal or rename whose `#[allow]` or comment says
/// "Glimt's own:"; a copy changes no file. The ways themselves are out of it too: a function that
/// is one of the acts (`rename_files`), whose callers check, each of `WAYS_OF_CHANGING_FILES`
/// by its file and name, and the tests. Before, the files that define the ways were left out
/// whole, so a removal after a long copy in one of them (a move to another disk) was never seen.
#[test]
fn destructive_acts_check_the_file_is_still_the_one_picked() {
    /// The functions that are the ways a user's file is changed, each in its file, with why
    /// it needs no check of its own. Every other function of their files is checked.
    const WAYS_OF_CHANGING_FILES: [(&str, &str, &str); 8] = [
        // A move or a copy, `put_file_until_stopped` with no stop; a call that moves is an act.
        ("files/file_io.rs", "put_file", "its callers check"),
        // The old name of a file just linked at its new one, or copied there and looked at
        // (`still_the_file_copied`), and a new name it just made, taken back.
        (
            "files/file_io.rs",
            "unlink_linked",
            "it removes names it looked at or made",
        ),
        ("files/file_io.rs", "unlink", "the unlink of unlink_linked"),
        // A document read and written again whole, a sidecar or a collection, not a file
        // picked: `replace_name_with` with nothing to look at before.
        ("files/file_io.rs", "replace_name", "it writes what it read"),
        (
            "files/trash.rs",
            "trash_by_hand",
            "the way `trash` takes a file there",
        ),
        // Undo's way out of the trash, to names that `rename_never_over` never goes over.
        (
            "files/trash.rs",
            "put_back",
            "it takes back what the trash holds",
        ),
        // Rename Many's steps, as its plan names them, settled after a cut or a lost answer.
        ("files/journal.rs", "finish_step", "a step of a plan"),
        ("files/journal.rs", "repair_step", "a step of a plan"),
    ];
    let sources = rust_sources();
    let tests = test_only_files(&sources);
    let mut wrong = Vec::new();
    for (file, text) in &sources {
        if tests.contains(file) {
            continue;
        }
        for (function, checks) in destructive_acts_in(text) {
            let is_a_way = (WAYS_OF_CHANGING_FILES.iter())
                .any(|&(f, name, _)| (f, name) == (file.as_str(), function.as_str()));
            if !checks && !is_a_way {
                wrong.push(format!("{file}: {function}"));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "a delete, trash, move, rename or replace that does not check the file is still the one \
         picked (file_io::PickedFiles, is_still_the_file):\n{}",
        wrong.join("\n")
    );
}

/// Returns the files of `sources` that are built only for the tests: a `mod` under
/// `#[cfg(test)]` in the file of its folder.
fn test_only_files(sources: &[(String, String)]) -> Vec<String> {
    let mut found = Vec::new();
    for (file, text) in sources {
        let folder = file.strip_suffix(".rs").filter(|_| file != "main.rs");
        let lines: Vec<&str> = text.lines().collect();
        for pair in lines.windows(2) {
            let module = (without_qualifiers(pair[1]).strip_prefix("mod "))
                .and_then(|m| m.strip_suffix(';'));
            if let Some(module) = module.filter(|_| pair[0].trim() == "#[cfg(test)]") {
                found.push(match folder {
                    Some(folder) => format!("{folder}/{module}.rs"),
                    None => format!("{module}.rs"),
                });
            }
        }
    }
    found
}

/// Returns the functions of `text` outside the tests that delete, trash, move, rename or replace
/// a user's file, each with whether it checks that the file is still the one picked or read.
// shortcut: a function counts as checked when it checks anywhere, so a branch that acts without
// the check passes with the rest (rename_undoable checked only for an Undo before 0.81). The
// PickedFiles type that every such act takes holds the branches; following each branch would
// need a parser, worth it if this class is found again.
fn destructive_acts_in(text: &str) -> Vec<(String, bool)> {
    const ACTS: [&str; 10] = [
        "remove_file(",
        "remove_dir_all(",
        "trash::trash(",
        "put_file_until_stopped(",
        "rename_files(",
        "rename_files_planned_in(",
        "replace_name_with(",
        "rename_never_over(",
        "rename_step(",
        "put_file_back(",
    ];
    const CHECKS: [&str; 5] = [
        "still_the_one_picked(",
        "is_still_the_file(",
        "same_fingerprint(",
        "replace_existing(",
        "same_file_by_opening(",
    ];
    const SIDECAR_FINDERS: [&str; 2] = ["sidecars_moving_with(", "sidecars_of("];
    let (lines, comments) = (code_lines(text), comment_lines(text));
    let end = tests_start(&lines);
    // A raw removal or rename whose `#[allow]` or comment says it is Glimt's own, on a line of
    // its own statement or on the comment and attribute lines just above it. The statement
    // starts after the line above that ends with `;`, `{`, `}` or `,`, or after a blank line.
    let is_own = |i: usize| {
        let mut start = i;
        while let Some(above) = start.checked_sub(1) {
            let code = lines[above].trim();
            let blank = code.is_empty() && comments[above].trim().is_empty();
            if blank || code.ends_with([';', '{', '}', ',']) {
                break;
            }
            start = above;
        }
        (start..=i).any(|j| comments[j].contains("Glimt's own:"))
    };
    let is_copy = |line: &str| line.contains(", true)");
    let mut found = Vec::new();
    let mut function = String::new();
    // Whether the function acts, acts on more than sidecars, checks, and finds sidecars.
    let (mut acts, mut acts_on_files, mut checks, mut finds_sidecars) =
        (false, false, false, false);
    for (i, line) in lines[..end]
        .iter()
        .map(String::as_str)
        .chain(["fn end"])
        .enumerate()
    {
        if let Some(name) = function_name(line) {
            let is_a_way = ACTS.contains(&format!("{function}(").as_str());
            if acts && !is_a_way {
                found.push((function, checks || (finds_sidecars && !acts_on_files)));
            }
            function = name.to_owned();
            (acts, acts_on_files, checks, finds_sidecars) = (false, false, false, false);
            continue;
        }
        let raw = ["remove_file(", "remove_dir_all(", "rename_never_over("]
            .iter()
            .any(|r| line.contains(r));
        let act = (ACTS.iter().any(|act| line.contains(act)) && !(raw && is_own(i)))
            || (line.contains("put_file(") && !is_copy(line));
        acts |= act;
        acts_on_files |= act && !names_a_sidecar(line);
        checks |= CHECKS.iter().any(|check| line.contains(check));
        finds_sidecars |= SIDECAR_FINDERS.iter().any(|finder| line.contains(finder));
    }
    found
}

/// Returns whether `line` names a sidecar: a word that starts with `side` (`side`, `sides`,
/// `sidecars`), not one that only holds it (`beside`, `inside`).
fn names_a_sidecar(line: &str) -> bool {
    (line.match_indices("side")).any(|(at, _)| !line[..at].ends_with(is_word_char))
}

/// The check above finds a delete, a trash, a move, a rename and a file put back with no look
/// at the file, also beside a look for its sidecars, and an act on files whose name only holds
/// `side`. It passes one that looks first, a removal or rename of Glimt's own file (also one
/// whose statement takes several lines), a copy, a function that only looks, an act on the
/// sidecars it finds, and a function that is one of the acts itself. A rename just below a
/// removal of Glimt's own file is not excused by that removal's reason.
#[test]
fn finds_a_destructive_act_without_its_check() {
    let text = "fn a() {\n    std::fs::remove_file(x)\n}\n\
        fn b() {\n    picked.still_the_one_picked(x)?;\n    trash::trash(&[x])\n}\n\
        fn c() {\n    crate::files::trash::trash(&[x])\n}\n\
        fn d() {\n    is_still_the_file(x, f)\n}\n\
        fn e() {\n    #[allow(clippy::disallowed_methods)] // Glimt's own: a socket\n    \
        std::fs::remove_file(x)\n}\n\
        fn f() {\n    put_file(x, y, true)\n}\n\
        fn g() {\n    put_file(x, y, false)\n}\n\
        fn h() {\n    let sides = xmp::sidecars_of(&[x]);\n    trash::trash(&[x])\n}\n\
        fn i() {\n    for (side, to) in xmp::sidecars_moving_with(x, y) {\n        \
        rename_never_over(&side, &to)?;\n    }\n}\n\
        fn j() {\n    file_io::rename_never_over(x, y)\n}\n\
        fn k() {\n    journal::rename_step(x, y)\n}\n\
        fn l() {\n    file_io::put_file_back(x, y, false)\n}\n\
        fn m() {\n    // Glimt's own: a plan made just now\n    rename_never_over(tmp, plan)\n}\n\
        fn n() {\n    let files_beside = xmp::sidecars_of(&[x]);\n    \
        trash::trash(&files_beside)\n}\n\
        fn o() {\n    // Glimt's own: a plan\n    let _ = std::fs::remove_file(plan);\n    \
        rename_never_over(x, y)\n}\n\
        fn p() {\n    #[allow(clippy::disallowed_methods)] // Glimt's own: an old folder\n    \
        if a\n        && std::fs::remove_dir_all(x).is_ok()\n    {\n    }\n}\n\
        fn rename_files_planned_in() {\n    journal::rename_step(x, y)\n}\n";
    assert_eq!(
        destructive_acts_in(text),
        [
            ("a".into(), false),
            ("b".into(), true),
            ("c".into(), false),
            ("g".into(), false),
            ("h".into(), false),
            ("i".into(), true),
            ("j".into(), false),
            ("k".into(), false),
            ("l".into(), false),
            ("n".into(), false),
            ("o".into(), false),
        ]
    );
}

/// A file is given as a path alone, with no identity from the list, only where no list knows
/// it: a Lightroom catalog names its photos by path, and an Undo holds fingerprints of its own
/// or puts back the marks of files it put back.
/// Anywhere else, a `ListedFile` made so would let a sidecar write go to whatever file has the
/// name by then, as if it were the file the user picked. A `ListedFile::new` given no identity
/// (`None`, or a default) makes the same, so it is found anywhere outside the tests.
#[test]
fn a_path_alone_is_given_only_where_no_list_knows_the_file() {
    const HOMES: &[(&str, &[(&str, &str)])] = &[(
        "not_from_the_list(",
        &[
            ("metadata/lightroom.rs", "import_lightroom"),
            ("files/file_ops.rs", "undo"),
            ("metadata/marks.rs", "restore"),
            ("support/test_support.rs", ""),
        ],
    )];
    let mut wrong = outside_their_homes(HOMES);
    let sources = rust_sources();
    let tests = test_only_files(&sources);
    for (file, text) in sources.iter().filter(|(file, _)| !tests.contains(file)) {
        let found = listed_files_made_without_identity(text);
        wrong.extend(found.into_iter().map(|line| format!("{file}:{line}")));
    }
    assert!(
        wrong.is_empty(),
        "a ListedFile made from a path alone (take it from the list, App::listed_file):\n{}",
        wrong.join("\n")
    );
}

/// Returns the lines of `text` outside the tests, counted from 1, where a `ListedFile::new` is
/// given no identity as its second argument: `None`, or a default.
fn listed_files_made_without_identity(text: &str) -> Vec<usize> {
    let lines = code_lines(text);
    let end = tests_start(&lines);
    let mut found = Vec::new();
    for (i, line) in lines[..end].iter().enumerate() {
        let Some(at) = line.find("ListedFile::new(") else {
            continue;
        };
        // The call's arguments, on this line and the lines after it, split at the commas
        // outside any brackets.
        let (mut depth, mut arguments, mut argument) = (0, Vec::new(), String::new());
        let call = line[at + "ListedFile::new".len()..].chars();
        for c in call.chain(lines[i + 1..end].iter().flat_map(|l| l.chars())) {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                _ => {}
            }
            if depth == 0 || (depth == 1 && c == ',') {
                arguments.push(std::mem::take(&mut argument));
                if depth == 0 {
                    break;
                }
            } else if depth > 1 || c != '(' {
                argument.push(c);
            }
        }
        let identity = arguments.get(1).map_or("", |a| a.trim());
        if identity == "None" || identity.ends_with("::None") || identity.ends_with("default()") {
            found.push(i + 1);
        }
    }
    found
}

/// The check above finds a `ListedFile::new` given `None` or a default for its identity, also
/// when the call takes several lines, but not one given the list's identity, nor one in the
/// tests.
#[test]
fn finds_a_listed_file_made_without_identity() {
    let text = "fn a() {\n    ListedFile::new(p, None, &Default::default())\n}\n\
        fn b() {\n    file_io::ListedFile::new(\n        p.to_path_buf(),\n        \
        Default::default(),\n        old,\n    )\n}\n\
        fn c() {\n    ListedFile::new(self.path(i), self.list.identity(i), old)\n}\n\
        fn d() {\n    ListedFile::new(p, Identity::default(), old)\n}\n\
        #[cfg(test)]\nmod tests {\n    fn e() {\n        ListedFile::new(p, None, old)\n    }\n}\n";
    assert_eq!(listed_files_made_without_identity(text), [2, 5, 15]);
}

/// The files built only for the tests are found by their `mod` lines, in a folder's file and in
/// main.rs.
#[test]
fn finds_the_files_built_only_for_tests() {
    let sources = [
        (
            "support.rs".into(),
            "#[cfg(test)]\npub(crate) mod rules;\npub mod x;\n".into(),
        ),
        ("main.rs".into(), "#[cfg(test)]\nmod checks;\n".into()),
    ];
    assert_eq!(test_only_files(&sources), ["support/rules.rs", "checks.rs"]);
}

/// A name counts as gone only when the system said that it is not there (NotFound).
/// `Path::exists`, `Path::is_file` and `is_dir`, and a look whose every error is taken as an
/// answer (`metadata(…).is_err()`, `.is_ok()`, `let Ok(m) = metadata(…) else`,
/// `.is_ok_and(|m| m.is_dir())`, `.ok().filter(|m| m.is_file())` or
/// `.map_or((false, false), |m| (m.is_file(), m.is_dir()))`, also of `is_symlink`, and any
/// other look that asks the kind without passing its error on with `?`), take a share
/// that dropped for a moment for a name that is gone, or for one that is no file or folder.
/// That way a sidecar was trashed while its photo stayed, a rename went over a file that it
/// could not look at, and a backup left out the folders it could not look at. So a name is
/// looked at only through `file_io::is_surely_gone`, `is_surely_there`, `is_surely_file`,
/// `is_surely_dir` and `is_link`, whose names or answers say which way a failed look counts.
#[test]
fn gone_means_not_found() {
    let (mut wrong, sources) = (Vec::new(), rust_sources());
    // The files built only for the tests, such as these rules, look at the repository's own.
    let tests = test_only_files(&sources);
    for (file, text) in sources.iter().filter(|(file, _)| !tests.contains(file)) {
        let found = looks_that_guess(text);
        wrong.extend(found.into_iter().map(|line| format!("{file}:{line}")));
    }
    assert!(
        wrong.is_empty(),
        "a name looked at with exists(), is_file(), is_dir() or metadata(…).is_err(), .is_ok(), \
         let Ok(…) = …, or asked its kind without `?`, which take any failed look for an answer; use \
         file_io::is_surely_gone, is_surely_there, is_surely_file, is_surely_dir or is_link:\n{}",
        wrong.join("\n")
    );
}

/// Returns the lines (from 1) of `text` outside the tests that ask whether a name is there in a
/// way that takes every failed look for an answer. A call split over two lines is found too.
fn looks_that_guess(text: &str) -> Vec<usize> {
    let lines = code_lines(text);
    let end = tests_start(&lines);
    let guesses = |i: usize| {
        let line = &lines[i];
        let next = lines.get(i + 1).map(|l| l.trim_start()).unwrap_or_default();
        let call = format!("{line}{}", if next.starts_with('.') { next } else { "" });
        let asks_kind = [".is_file()", ".is_dir()", ".is_symlink()"]
            .iter()
            .any(|c| call.contains(c));
        line.contains(".exists()")
            || ["Path::exists", "Path::is_file", "Path::is_dir"]
                .iter()
                .any(|f| mentions(line, f))
            || (line.contains("metadata(")
                && ([".is_err()", ".is_ok()", "let Ok("]
                    .iter()
                    .any(|c| call.contains(c))
                    || (asks_kind && !passes_each_error_on(&call))))
            || asks_a_path_its_kind(line)
    };
    (0..end).filter(|&i| guesses(i)).map(|i| i + 1).collect()
}

/// Returns whether each look at the metadata in `call` passes its error on to the caller, with
/// `?` right after it. A look whose kind is asked in any other way gives an answer for a failed
/// look too (`is_ok_and`, `map_or`, `.ok().is_some_and`, `unwrap_or`, a `matches!` of
/// `Ok(m)`), however it is spelled.
fn passes_each_error_on(call: &str) -> bool {
    (call.match_indices("metadata(")).all(|(at, _)| {
        let mut depth = 0;
        let close = call[at + "metadata".len()..]
            .char_indices()
            .find(|&(_, c)| {
                depth += match c {
                    '(' => 1,
                    ')' => -1,
                    _ => 0,
                };
                depth == 0
            });
        let Some((close, _)) = close else {
            return false;
        };
        let after = &call[at + "metadata".len() + close + 1..];
        after.starts_with('?')
    })
}

/// Returns whether `line` asks a path whether it is a file or a folder (`Path::is_file` or
/// `is_dir`), which answers no when the look fails. A metadata or a file type, which has been
/// read already, is asked the same way, and is told apart by how it is named: a call of
/// `metadata`, `symlink_metadata` or `file_type`, or a value named `m`, `meta` or `kind`.
fn asks_a_path_its_kind(line: &str) -> bool {
    let is_read_already = |before: &str| {
        let before = before.trim_end_matches('?');
        let Some(inside) = before.strip_suffix(')') else {
            let name = &before[before.trim_end_matches(is_word_char).len()..];
            return ["m", "meta", "kind"].contains(&name);
        };
        // The name of the call whose `)` ends `before`.
        let mut depth = 1;
        let open = inside.char_indices().rev().find(|&(_, c)| {
            depth += match c {
                ')' => 1,
                '(' => -1,
                _ => 0,
            };
            depth == 0
        });
        let called = &inside[..open.map_or(0, |(at, _)| at)];
        let name = &called[called.trim_end_matches(is_word_char).len()..];
        ["metadata", "symlink_metadata", "file_type"].contains(&name)
    };
    [".is_file()", ".is_dir()"]
        .iter()
        .any(|ask| (line.match_indices(ask)).any(|(at, _)| !is_read_already(&line[..at])))
}

/// The check above finds `exists`, also as a function value, and a look at the metadata whose
/// errors count as gone or as there, also when split over two lines. It leaves the helpers, a
/// metadata read that passes its error on, and `try_exists`, whose `Ok(false)` is NotFound only.
/// A `let Ok(…) = metadata(…) else`, which takes every failed look for no file, is found too:
/// `create_temp_beside` made a new file so, without the mode of the file it replaced.
#[test]
fn finds_a_look_that_guesses() {
    let text = "fn a() {\n    if p.exists() {}\n    let gone = std::fs::symlink_metadata(p)\n        \
        .is_err();\n    let there = q.symlink_metadata().is_ok();\n    \
        files.filter(Path::exists);\n    let fine = is_surely_gone(p);\n    \
        let m = std::fs::metadata(p)?;\n    p.try_exists()\n    \
        let Ok(old) = std::fs::metadata(p) else {\n    if let Ok(m) = p.metadata() {}\n}\n";
    assert_eq!(looks_that_guess(text), [2, 3, 5, 6, 10, 11]);
}

/// The check above finds a look whose failure is taken for "no link" or "neither a file nor a
/// folder", also when its default comes first (`map_or`) or it is filtered (`.ok().filter`),
/// and in any other spelling that does not pass the error on (`.ok().is_some_and`, `unwrap_or`,
/// `matches!`). It leaves a size whose look failed, and a look whose error goes to the caller.
/// Before, only the listed spellings were found, and `matches!(metadata(p), Ok(m) if …)` passed.
#[test]
fn finds_a_failed_look_taken_for_its_kind() {
    let text = "fn a() {\n    let l = std::fs::symlink_metadata(p).is_ok_and(|m| m.is_symlink());\n    \
        let (t, d) = std::fs::metadata(p).map_or((false, false), |m| (true, m.is_dir()));\n    \
        let size = std::fs::metadata(p).map_or(0, |m| m.len());\n    let l = is_link(p)?;\n    \
        let f = std::fs::metadata(p).ok().filter(|m| m.is_file());\n    \
        let n = std::fs::metadata(p).ok().map(|m| m.len());\n    \
        let d = std::fs::metadata(p).ok().is_some_and(|m| m.is_dir());\n    \
        let f = std::fs::metadata(p).ok().map(|m| m.is_file()).unwrap_or(false);\n    \
        let f = matches!(std::fs::metadata(p), Ok(m) if m.is_file());\n    \
        let d = std::fs::metadata(&p.join(q))?.is_dir();\n}\n";
    assert_eq!(looks_that_guess(text), [2, 3, 6, 8, 9, 10]);
}

/// The check above finds a path asked whether it is a file or a folder, also as a function value
/// and as the metadata's look spelled out. It leaves a metadata or file type read already.
#[test]
fn finds_a_path_asked_its_kind() {
    let text = "fn a() {\n    if dir.is_dir() {}\n    let f = d.join(name).is_file();\n    \
        p.parent().is_some_and(Path::is_dir);\n    \
        std::fs::metadata(p).is_ok_and(|m| m.is_file());\n    if m.is_dir() {}\n    \
        let f = meta.is_file() || kind.is_dir();\n    let d = std::fs::metadata(p)?.is_dir();\n    \
        e.file_type().is_ok_and(|t| t.is_file());\n    let f = lock.metadata()?.is_file();\n    \
        let x = is_surely_dir(p);\n}\n";
    assert_eq!(looks_that_guess(text), [2, 3, 4, 5, 9]);
}

/// A file is locked only through `file_io::open_and_lock` and `open_if_unlocked`, which open it
/// for reading and writing, never truncate it, never follow a link and never wait on a pipe
/// planted in its place. Opened to be read only, a plan of Rename Many or a log of Move was
/// never locked on an NFS home, where a flock needs a file open for writing, so none was put
/// right after a kill. Opened with `File::create`, a link planted in Glimt's runtime folder
/// emptied the file of the user's that it pointed to. So a lock taken in any other function is
/// found: a `lock`, a `try_lock` or their shared forms, a `flock` or an `fcntl` lock. A mutex
/// is locked in the one way for it too, `threads::lock_even_if_poisoned` (clippy.toml).
#[test]
fn files_are_locked_in_one_place() {
    const ONE_WAYS: [(&str, &str); 3] = [
        ("files/file_io.rs", "open_and_lock"),
        ("files/file_io.rs", "open_if_unlocked"),
        ("support/threads.rs", "lock_even_if_poisoned"),
    ];
    let mut wrong = Vec::new();
    for (file, text) in rust_sources() {
        let found = (functions_that_lock(&text).into_iter())
            .filter(|f| !ONE_WAYS.contains(&(file.as_str(), f.as_str())));
        wrong.extend(found.map(|f| format!("{file}: {f}")));
    }
    assert!(
        wrong.is_empty(),
        "a lock taken outside file_io::open_and_lock and open_if_unlocked:\n{}",
        wrong.join("\n")
    );
}

/// Returns the functions of `text` outside the tests that take a lock, of a file or a mutex. A
/// lock is found by its name, whatever its arguments: called as a method (`f.lock()`), by the
/// path of a type (`File::try_lock(&f)`, also as a value in `.map(File::lock)`), or as a lock
/// of the C library (`flock`, `lockf`, an `fcntl` lock) by any path. A function of a module that
/// only shares the name, such as `config::lock()`, takes no lock.
fn functions_that_lock(text: &str) -> Vec<String> {
    const LOCKS: [&str; 4] = ["lock", "try_lock", "lock_shared", "try_lock_shared"];
    const C_LOCKS: [&str; 6] = [
        "flock(",
        "lockf(",
        "F_SETLK",
        "F_OFD_SETLK",
        "F_LOCK",
        "F_TLOCK",
    ];
    let lines = code_lines(text);
    let end = tests_start(&lines);
    let mut found: Vec<String> = Vec::new();
    let mut function = String::new();
    for line in &lines[..end] {
        if let Some(name) = function_name(line) {
            function = name.to_owned();
        }
        let takes = |at: usize, name: &str| {
            let (before, after) = (&line[..at], &line[at + name.len()..]);
            // The segment before `::`, which names a type when it starts with a capital.
            let owner = before.strip_suffix("::");
            let owner = owner.map(|b| &b[b.trim_end_matches(is_word_char).len()..]);
            let is_by_type = owner.is_some_and(|o| o.starts_with(char::is_uppercase));
            !before.ends_with(is_word_char)
                && ((before.ends_with('.') && after.starts_with('('))
                    || (is_by_type && after.starts_with(['(', ')', ','])))
        };
        let locks = (LOCKS.iter())
            .any(|name| (line.match_indices(name)).any(|(at, _)| takes(at, name)))
            || (C_LOCKS.iter()).any(|call| {
                (line.match_indices(call)).any(|(at, _)| !line[..at].ends_with(is_word_char))
            });
        if locks && found.last() != Some(&function) {
            found.push(function.clone());
        }
    }
    found
}

/// The check above finds a `try_lock`, a `flock` and a shared lock, a lock called by the path
/// of its type or passed as a value, and `lockf`. It leaves a lock taken through
/// `open_and_lock`, a function of a module that shares the name, and a longer name. Before, a
/// lock was found only as a method, so `File::try_lock(&f)` and `libc::lockf` passed unseen.
#[test]
fn finds_a_lock_taken_by_hand() {
    let text = "fn a() {\n    let f = open_regular(p)?;\n    f.try_lock()?;\n}\n\
        fn b() {\n    let f = open_and_lock(p)?;\n}\n\
        fn c() {\n    libc::flock(fd, libc::LOCK_EX);\n}\n\
        fn d() {\n    f.lock_shared()\n}\n\
        fn e() {\n    std::fs::File::try_lock(&f)?;\n}\n\
        fn g() {\n    File::lock(&f)?;\n}\n\
        fn h() {\n    files.iter().map(File::lock);\n}\n\
        fn i() {\n    libc::lockf(fd, libc::F_LOCK, 0);\n}\n\
        fn j() {\n    crate::files::config::lock();\n    xmp::lock_sidecar(p);\n    \
        f.unlock()?;\n    update_locked_sidecar(&lock, p);\n}\n";
    let found = functions_that_lock(text);
    assert_eq!(found, ["a", "c", "d", "e", "g", "h", "i"]);
}

/// The functions of the C library are called through the `libc` crate, never declared by hand
/// in an `extern "C"` block. A declaration written by hand is a guess at the system's types,
/// and `time_t`, `struct tm` and `struct timespec` differ on 32-bit systems. A function pointer
/// that C calls (`extern "C" fn`, a signal handler) is not a declaration, and stays.
#[test]
fn foreign_functions_come_from_libc() {
    let mut wrong = Vec::new();
    for (file, text) in rust_sources() {
        let found = foreign_declarations(&text);
        wrong.extend(found.into_iter().map(|line| format!("{file}:{line}")));
    }
    assert!(
        wrong.is_empty(),
        "a C function declared by hand; call it through libc:\n{}",
        wrong.join("\n")
    );
}

/// Returns the lines (from 1) of `text` outside the tests that open an `extern` block.
fn foreign_declarations(text: &str) -> Vec<usize> {
    let lines = code_lines(text);
    let end = tests_start(&lines);
    let opens_block = |line: &String| {
        let words: Vec<&str> = line.split_whitespace().collect();
        words.windows(2).any(|w| w == ["extern", "{"])
    };
    (0..end)
        .filter(|&i| opens_block(&lines[i]))
        .map(|i| i + 1)
        .collect()
}

/// The check above finds an `extern "C"` block, and leaves a function that C calls.
#[test]
fn finds_a_foreign_declaration() {
    let text = "unsafe extern \"C\" {\n    fn kill(pid: i32, signal: i32) -> i32;\n}\n\
        extern \"C\" fn on_signal(_signal: i32) {}\n";
    assert_eq!(foreign_declarations(text), [1]);
}

/// Returns the functions of `text` outside the tests that rename a file, also by a move through
/// `put_file`, each with whether it syncs the folder, itself or through `write_beside`.
// shortcut: a function counts as synced when it syncs anywhere, so a branch that renames
// without a sync passes with the rest. put_file's move on one disk was such a branch, and is
// now its callers' to sync; following each branch would need a parser, worth it if this class
// is found again.
fn renames_in(text: &str) -> Vec<(String, bool)> {
    let lines = code_lines(text);
    let end = tests_start(&lines);
    let mut found = Vec::new();
    let mut function = String::new();
    let (mut renames, mut synced) = (false, false);
    for line in lines[..end].iter().map(String::as_str).chain(["fn end"]) {
        if let Some(name) = function_name(line) {
            if renames {
                found.push((function, synced));
            }
            function = name.to_owned();
            (renames, synced) = (false, false);
            continue;
        }
        let calls = ["rename_never_over(", "fs::rename(", "rename_noreplace("];
        renames |= calls.iter().any(|call| line.contains(call));
        let moves = ["put_file(", "put_file_until_stopped("];
        renames |= moves.iter().any(|call| line.contains(call))
            && ![", true)", ", true,"]
                .iter()
                .any(|copy| line.contains(copy));
        let syncs = [
            "sync_folder_of(",
            "sync_folder(",
            "sync_file_system_of(",
            "write_beside(",
        ];
        synced |= syncs.iter().any(|call| line.contains(call));
    }
    found
}

/// The check above finds a rename by any of the three calls, and a move through `put_file`,
/// and sees it synced by each way. A write without its folder synced is not, nor is a copy.
#[test]
fn finds_a_rename_without_its_folder_synced() {
    let text = "fn a() {\n    rename_never_over(x, y)?;\n}\nfn b() {\n    std::fs::rename(x, y)?;\n\
        sync_folder_of(y)?;\n}\nfn c() {\n    write_beside(t, p, w, |t| std::fs::rename(t, p))\n}\n\
        fn d() {\n    rename_noreplace(x, y)\n}\nfn e() {\n    put_file(x, y, true)\n}\n\
        fn f() {\n    write_beside_without_folder_sync(t, p, w, |t| std::fs::rename(t, p))\n}\n\
        fn g() {\n    std::fs::rename(x, y)?;\n    sync_folder(z)?;\n}\n\
        fn h() {\n    put_file(x, y, is_copy)\n}\n\
        fn i() {\n    put_file_until_stopped(x, y, true, &stop)\n}\n";
    let found = renames_in(text);
    let names: Vec<(&str, bool)> = found.iter().map(|(f, s)| (f.as_str(), *s)).collect();
    assert_eq!(
        names,
        [
            ("a", false),
            ("b", true),
            ("c", true),
            ("d", false),
            ("f", false),
            ("g", true),
            ("h", false)
        ]
    );
}

/// The lints in Cargo.toml's `[lints]` that hold Glimt's rules on unsafe code and on errors.
/// Each must stay at deny: one taken out or lowered to warn lets an undocumented unsafe block or
/// a panic on an error back in without a word from `cargo clippy`. No module, `impl` or `mod`
/// turns one off for all its code either, nor clippy's `disallowed_methods`, which refuses the
/// raw calls of clippy.toml: an exception is an allow on the narrowest statement or function,
/// with its reason. Every call of `REFUSED_CALLS` stays in clippy.toml, `Mutex::lock` among
/// them, so a lock goes through `threads::lock_even_if_poisoned`.
#[test]
fn the_lints_stay_on() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let cargo = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    let mut wrong = Vec::new();
    for (section, lint) in [
        ("[lints.rust]", "unsafe_code"),
        ("[lints.clippy]", "undocumented_unsafe_blocks"),
        ("[lints.clippy]", "unwrap_used"),
        ("[lints.clippy]", "expect_used"),
    ] {
        let level = cargo.split_once(section).and_then(|(_, rest)| {
            let line = (rest.lines().skip(1))
                .take_while(|l| !l.starts_with('['))
                .find(|l| l.split('=').next().map(str::trim) == Some(lint))?;
            Some(line.split_once('=')?.1.trim().trim_matches('"').to_owned())
        });
        if !matches!(level.as_deref(), Some("deny" | "forbid")) {
            wrong.push(format!(
                "Cargo.toml: {section} {lint} is {level:?}, not deny"
            ));
        }
    }
    for (file, text) in rust_sources() {
        for line in lints_turned_off_widely(&text) {
            wrong.push(format!(
                "{file}:{line}: a lint turned off for a whole module or block"
            ));
        }
    }
    let clippy = std::fs::read_to_string(root.join("clippy.toml")).unwrap();
    for call in REFUSED_CALLS {
        if !is_refused(&clippy, call) {
            wrong.push(format!("clippy.toml: {call} is no longer refused"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Returns whether clippy.toml's text `clippy` refuses the call `call`, on a line that is not
/// commented out with `#`.
fn is_refused(clippy: &str, call: &str) -> bool {
    let entry = format!("path = \"{call}\"");
    (clippy.lines()).any(|l| !l.trim_start().starts_with('#') && l.contains(&entry))
}

/// The check above takes a call as refused only on a line that is not commented out.
#[test]
fn a_call_commented_out_of_clippy_toml_is_not_refused() {
    let clippy = "disallowed-methods = [\n    { path = \"std::fs::write\", reason = \"…\" },\n\
        #    { path = \"std::fs::rename\", reason = \"…\" },\n]\n";
    assert!(is_refused(clippy, "std::fs::write"));
    assert!(!is_refused(clippy, "std::fs::rename"));
}

/// Returns the lines of `text` that turn off one of the lints of `the_lints_stay_on` for a
/// whole module (`#![allow(…)]`, outside the tests), or for a whole `impl` or `mod`.
fn lints_turned_off_widely(text: &str) -> Vec<usize> {
    const LINTS: [&str; 5] = [
        "unsafe_code",
        "undocumented_unsafe",
        "unwrap_used",
        "expect_used",
        "disallowed_methods",
    ];
    let lines = code_lines(text);
    let mut found = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let line = line.trim_start();
        if !line.starts_with("#") || !LINTS.iter().any(|lint| line.contains(lint)) {
            continue;
        }
        // The item the attribute stands on, after any other attributes.
        let item = (lines[i + 1..].iter())
            .map(|l| without_qualifiers(l.trim_start()))
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .unwrap_or("");
        let is_for_a_block = item.starts_with("impl") || item.starts_with("mod ");
        let is_wide = match line.starts_with("#![") {
            true => !line.contains("(test,"),
            false => is_for_a_block,
        };
        if is_wide {
            found.push(i + 1);
        }
    }
    found
}

/// The check above finds a lint turned off for a module, for an `impl` (also behind another
/// attribute) and for a `mod`. It leaves one turned off for a statement or a function, and the
/// tests' own `cfg_attr(test, …)`.
#[test]
fn finds_a_lint_turned_off_widely() {
    let text = "#![allow(clippy::disallowed_methods)]\n\
        #![cfg_attr(test, allow(clippy::unwrap_used))]\n\
        #[allow(clippy::disallowed_methods)]\nimpl crate::App {\n}\n\
        #[allow(clippy::unwrap_used)]\n#[cfg(unix)]\nunsafe impl Send for A {\n}\n\
        #[allow(clippy::expect_used)]\nmod inner {\n}\n\
        #[allow(clippy::disallowed_methods)] // a reason\nfn f() {\n    \
        #[allow(clippy::unwrap_used)] // a reason\n    let x = y.unwrap();\n}\n";
    assert_eq!(lints_turned_off_widely(text), [1, 3, 6, 10]);
}

/// Every `#[allow(…)]` or `#[expect(…)]` outside the tests says why it is there, in a comment on
/// its own line or in a comment alone on the next line. A reader then knows what the exception
/// is for, and when it may go. A comment before the attribute, or one after the code on the
/// next line, is about other code as often as not, so neither counts.
#[test]
fn every_allow_gives_its_reason() {
    let mut wrong = Vec::new();
    for (file, text) in rust_sources() {
        let lines = allows_without_reason(&text);
        wrong.extend(lines.iter().map(|line| format!("{file}:{line}")));
    }
    assert!(
        wrong.is_empty(),
        "an #[allow(…)] without its reason (say why in a comment on its line):\n{}",
        wrong.join("\n")
    );
}

/// Returns the lines of `text` outside the tests with an attribute that allows or expects a
/// lint and no comment that gives its reason: one on that line, or a plain comment (not a doc
/// comment) alone on the next.
fn allows_without_reason(text: &str) -> Vec<usize> {
    let (code, comments) = (code_lines(text), comment_lines(text));
    let comment = |j: usize| comments.get(j).map_or("", |c| c.trim());
    let is_plain = |c: &str| !c.is_empty() && !c.starts_with("///") && !c.starts_with("//!");
    let is_reason_alone =
        |j: usize| code.get(j).is_some_and(|c| c.trim().is_empty()) && is_plain(comment(j));
    (0..tests_start(&code))
        .filter(|&i| {
            let line = code[i].trim_start();
            line.starts_with('#')
                && (line.contains("allow(") || line.contains("expect("))
                && comment(i).is_empty()
                && !is_reason_alone(i + 1)
        })
        .map(|i| i + 1)
        .collect()
}

/// The check above finds an allow or expect with no reason, also behind `cfg_attr`, under a doc
/// comment, under a comment before it and over code with a comment after it. It passes one with
/// its reason on its line or alone on the next.
#[test]
fn finds_an_allow_without_its_reason() {
    let text = "#[allow(dead_code)]\nfn a() {}\n/// Does b.\n\
        #[cfg_attr(test, allow(unused))]\nfn b() {}\n#[allow(unused)] // a reason\nfn c() {}\n\
        #[allow(unused)]\n// a reason\nfn d() {}\n// a comment about e\n#[allow(unused)]\n\
        fn e() {}\n#[expect(unused)]\nfn f() {}\n#[allow(unused)]\nfn g() {} // about g\n\
        #[expect(unused)] // a reason\nfn h() {}\n\
        #[cfg(test)]\nmod tests {\n    #[allow(unused)]\n    fn t() {}\n}\n";
    assert_eq!(allows_without_reason(text), [1, 4, 12, 14, 16]);
}

/// Returns the lines of the statements of `text` outside the tests that drop the result of one
/// of file_io's writers, with `let _ =` (or `let _: T =`), `_ =`, `.ok();`,
/// `.unwrap_or_default();`, `.unwrap_or(());`, `drop(…)`, `let _name =` where `_name` is not
/// used again, or `if let Ok(…) =` with no `else`. A raw change on the disk of the standard
/// library's, or an image encoded (`RAW_WRITES`), dropped so is found too, unless a comment in
/// the statement or in the comment lines just above it says why its failure is not told, with
/// "Not told:". An encoder's failure dropped left a printed page blank without a word.
fn dropped_writes_in(text: &str) -> Vec<usize> {
    // `create(` is any create, by its path or its receiver: `File::create`, and `DirBuilder`'s,
    // which makes a folder as `create_dir_all` does.
    const RAW_WRITES: [&str; 12] = [
        "create(",
        "remove_file(",
        "remove_dir(",
        "remove_dir_all(",
        "create_dir(",
        "create_dir_all(",
        "set_permissions(",
        "hard_link(",
        "fs::symlink(",
        "encode_image(",
        "write_image(",
        "write_to(",
    ];
    const WRITERS: [&str; 17] = [
        // collections::edit and edit_open_collection, which write through replace_name.
        "edit(",
        "edit_open_collection(",
        "sync_folder_of(",
        "sync_folder(",
        "write_beside_without_folder_sync(",
        "write_beside_without_sync(",
        "sync_file_system_of(",
        "rename_never_over(",
        "put_file(",
        "edit_shared(",
        "replace_file(",
        "replace_existing(",
        "replace_name(",
        "replace_name_with(",
        "write_beside(",
        "create_file(",
        // A removal of Glimt's own plan, log or state file, which the next start would act on
        // again if it stayed.
        "remove_own(",
    ];
    const KEYWORDS: [&str; 7] = ["if", "while", "match", "return", "let", "else", "for"];
    let lines = code_lines(text);
    let comments = comment_lines(text);
    let end = tests_start(&lines);
    let mut found = Vec::new();
    let (mut first_line, mut statement) = (0, String::new());
    // How deep the braces go, the depths of the blocks of an `if let Ok(…) =` around a writer
    // with their first lines, and the line of such a block just closed, which an `else` must
    // follow.
    let (mut depth, mut if_ok_blocks, mut needs_else) = (0usize, Vec::new(), None);
    for (i, line) in lines[..end].iter().enumerate() {
        for c in line.chars().chain([' ']) {
            if !matches!(c, ';' | '{' | '}') {
                if statement.trim().is_empty() {
                    first_line = i + 1;
                }
                statement.push(c);
                continue;
            }
            let words = without_attributes(statement.trim());
            if let Some(line) = needs_else.take()
                && !words.starts_with("else")
            {
                found.push(line);
            }
            let is_writer = |writer: &&str| words.contains(writer);
            match c {
                '{' => {
                    depth += 1;
                    if words.starts_with("if let Ok(") && WRITERS.iter().any(is_writer) {
                        if_ok_blocks.push((depth, first_line));
                    }
                }
                '}' => {
                    if if_ok_blocks.last().is_some_and(|(at, _)| *at == depth) {
                        needs_else = if_ok_blocks.pop().map(|(_, line)| line);
                    }
                    depth = depth.saturating_sub(1);
                }
                _ => {}
            }
            let starts_with_keyword = KEYWORDS.contains(&words.split(' ').next().unwrap_or(""));
            let dropped = words.starts_with("let _ =")
                || words.starts_with("let _:")
                || words.starts_with("_ =")
                || (c == ';'
                    && (words.ends_with(".unwrap_or_default()")
                        || words.ends_with(".unwrap_or(())")))
                || is_unused_binding(words, text)
                || (c == ';'
                    && words.ends_with(".ok()")
                    && !words.contains('=')
                    && !starts_with_keyword);
            // The statement's lines, and the lines of comment alone just above it.
            let says_why = || {
                let above = (0..first_line.saturating_sub(1))
                    .rev()
                    .take_while(|&j| lines[j].trim().is_empty() && !comments[j].trim().is_empty())
                    .last()
                    .unwrap_or(first_line.saturating_sub(1));
                (above..=i).any(|j| comments.get(j).is_some_and(|c| c.contains("Not told:")))
            };
            let drops_a_raw_write = (dropped && RAW_WRITES.iter().any(is_writer))
                || RAW_WRITES.iter().any(|w| is_dropped(words, w));
            if (dropped && WRITERS.iter().any(is_writer))
                || WRITERS.iter().any(|w| is_dropped(words, w))
                || (drops_a_raw_write && !says_why())
            {
                found.push(first_line);
            }
            statement.clear();
        }
    }
    found
}

/// Returns `statement` without the attributes in front of it (`#[allow(…)]`, `#[cfg(…)]`), so
/// that a statement under one is read as the statement it is.
fn without_attributes(statement: &str) -> &str {
    let mut rest = statement;
    while let Some(inside) = rest.strip_prefix("#[") {
        let mut depth = 1;
        let end = inside.find(|c| {
            depth += match c {
                '[' => 1,
                ']' => -1,
                _ => 0,
            };
            depth == 0
        });
        let Some(end) = end else {
            return rest;
        };
        rest = inside[end + 1..].trim_start();
    }
    rest
}

/// Returns whether `statement` binds a name that starts with `_` (`let _name = …`) that `text`
/// uses nowhere else.
fn is_unused_binding(statement: &str, text: &str) -> bool {
    let Some(rest) = statement.strip_prefix("let _") else {
        return false;
    };
    let name_length = rest.find(|c: char| !c.is_alphanumeric() && c != '_');
    let Some(name) = name_length.filter(|&n| n > 0).map(|n| &statement[4..5 + n]) else {
        return false;
    };
    let is_word_at = |at: usize| {
        let around = [
            text[..at].chars().next_back(),
            text[at + name.len()..].chars().next(),
        ];
        !around
            .into_iter()
            .flatten()
            .any(|c| c.is_alphanumeric() || c == '_')
    };
    text.match_indices(name)
        .filter(|(at, _)| is_word_at(*at))
        .count()
        < 2
}

/// Returns whether `statement` drops the result of `writer` with `drop(…)`, also through a path
/// (`drop(file_io::put_file(…))`).
fn is_dropped(statement: &str, writer: &str) -> bool {
    statement.match_indices(writer).any(|(at, _)| {
        let path_chars = |c: char| c.is_alphanumeric() || c == '_' || c == ':';
        statement[..at]
            .trim_end_matches(path_chars)
            .ends_with("drop(")
    })
}

/// A write that fails is told, never dropped: the result of each of file_io's writers is
/// handled, or passed on with `?`. A copy, a sidecar or one of Glimt's own files that was not
/// written would otherwise look saved. `write_atomic` and `write_later` tell their failures
/// themselves.
#[test]
fn write_results_are_not_dropped() {
    let mut wrong = Vec::new();
    for (file, text) in rust_sources() {
        wrong.extend(
            dropped_writes_in(&text)
                .iter()
                .map(|line| format!("{file}:{line}")),
        );
    }
    assert!(
        wrong.is_empty(),
        "the result of a write dropped (handle it, or pass it on with ?):\n{}",
        wrong.join("\n")
    );
}

/// The check above finds a dropped write in each of its forms, also over several lines, in a
/// match arm and under an attribute, and leaves a write that is handled alone, and a bound name
/// that is used.
#[test]
fn finds_a_dropped_write() {
    let text = "fn a() {\n    let _ = replace_file(p, b);\n    _ = create_file(p, b);\n    \
        put_file(a, b,\n        true).ok();\n    let _ =\n        crate::files::file_io::\
        rename_never_over(a, b);\n    if let Err(e) = replace_name(p, b) {\n        say(e);\n    \
        }\n    replace_file(p, b)?;\n    let made = create_file(p, b).ok();\n    \
        let _ = tx.send(1);\n    drop(file_io::put_file(a, b, true));\n    \
        let _copied = put_file(a, b, true);\n    let _kept = put_file(a, b, true);\n    \
        say(_kept);\n    let _held = file_lock(p);\n    match x {\n        \
        A => drop(sync_folder(p)),\n        B => drop(thumbnail(p)),\n    }\n    \
        if let Ok(_) = replace_name(p, b) {\n        noted();\n    }\n    \
        put_file(a, b, true).unwrap_or_default();\n    \
        let _: std::io::Result<()> = create_file(p, b);\n    \
        if let Ok(()) = replace_file(p, b) {\n        noted();\n    } else {\n        \
        say();\n    }\n    let n = text.len().unwrap_or_default();\n}\n";
    assert_eq!(
        dropped_writes_in(text),
        [2, 3, 4, 6, 14, 15, 20, 23, 26, 27]
    );
    // A removal of Glimt's own file, whose failure must be told too.
    let removals = "fn a() {\n    let _ = remove_own(plan);\n    remove_own_or_tell(plan);\n}\n";
    assert_eq!(dropped_writes_in(removals), [2]);
    // A raw change on the disk, dropped in each form, unless a comment says why it is not told.
    let raw = "fn a() {\n    let _ = std::fs::remove_file(p);\n    \
        drop(std::fs::create_dir_all(d));\n    std::fs::set_permissions(p, m).ok();\n    \
        // Not told: the write into it that follows tells.\n    \
        drop(std::fs::create_dir_all(d));\n    \
        let _ = std::fs::remove_dir_all(d); // Not told: a folder of its own\n    \
        std::fs::remove_file(p)?;\n    \
        let _ = JpegEncoder::new(&mut jpeg).encode_image(&flat);\n    \
        #[allow(clippy::disallowed_methods)] // a reason for the allow\n    \
        let _ = std::fs::remove_file(plan);\n    #[cfg(unix)]\n    \
        let _ = replace_file(p, b);\n    #[allow(clippy::disallowed_methods)]\n    \
        // Not told: a folder of its own\n    let _ = std::fs::remove_dir_all(d);\n}\n";
    assert_eq!(dropped_writes_in(raw), [2, 3, 4, 9, 10, 12]);
    // A folder made with a DirBuilder, by its path or through a name, over several lines.
    let builder = "fn a() {\n    let _ = std::fs::DirBuilder::new()\n        .recursive(true)\n        \
        .create(d);\n    let b = DirBuilder::new();\n    b.create(d).ok();\n}\n";
    assert_eq!(dropped_writes_in(builder), [2, 6]);
}

/// A background thread's answer is held in a `threads::Receiving`, never in a bare
/// `Option<Receiver>`, because `Receiving` tells once when the thread is gone, and a bare one
/// waits for an answer that never comes.
#[test]
fn answers_are_received_through_receiving() {
    let mut wrong = Vec::new();
    let sources: Vec<(String, String)> = (rust_sources().into_iter())
        .filter(|(file, _)| !file.ends_with("support/threads.rs"))
        .collect();
    for (file, text) in &sources {
        wrong.extend(
            bare_receivers_in(text)
                .iter()
                .map(|line| format!("{file}:{line}")),
        );
    }
    wrong.extend(receivers_held_in_options(&sources));
    assert!(
        wrong.is_empty(),
        "a bare Option<Receiver> (hold it in a threads::Receiving):\n{}",
        wrong.join("\n")
    );
}

/// Returns the places in `sources` outside the tests, as "file:line", where an `Option` holds a
/// struct with a bare `Receiver` field: the struct comes and goes as an answer held in an
/// `Option` would, and its receiver is a bare one held in an `Option` all the same.
fn receivers_held_in_options(sources: &[(String, String)]) -> Vec<String> {
    let mut holding = Vec::new();
    for (_, text) in sources {
        let lines = code_lines(text);
        let lines = &lines[..tests_start(&lines)];
        for (i, line) in lines.iter().enumerate() {
            let (Some(name), Some(end)) = (struct_name(line), block_end(lines, i)) else {
                continue;
            };
            let is_receiver = |l: &String| {
                let bare = l.replace("std::sync::", "").replace("mpsc::", "");
                bare.split_once(": ")
                    .is_some_and(|(_, t)| t.starts_with("Receiver<"))
            };
            if lines[i + 1..end].iter().any(is_receiver) {
                holding.push(name.to_owned());
            }
        }
    }
    let mut found = Vec::new();
    for (file, text) in sources {
        let lines = code_lines(text);
        for (i, line) in lines[..tests_start(&lines)].iter().enumerate() {
            let held = line.split_once(": ").map(|(_, t)| part_held_by(t).1);
            let in_option = line.contains(": Option<");
            if in_option && held.is_some_and(|h| holding.iter().any(|n| n == h)) {
                found.push(format!("{file}:{}", i + 1));
            }
        }
    }
    found
}

/// The check above finds a struct with a bare `Receiver` held in an `Option`, also in two, and
/// leaves one held for good and a struct whose answer is in a `Receiving`.
#[test]
fn finds_a_receiver_inside_a_struct_held_in_an_option() {
    let text = "struct Check {\n    found: mpsc::Receiver<u8>,\n}\nstruct Kept {\n    \
        answer: Receiving<u8>,\n}\nstruct App {\n    check: Option<Option<Check>>,\n    \
        print: Option<crate::features::print::Check>,\n    held: Check,\n    \
        kept: Option<Kept>,\n}\n";
    let sources = [("a.rs".to_owned(), text.to_owned())];
    assert_eq!(receivers_held_in_options(&sources), ["a.rs:8", "a.rs:9"]);
}

/// Returns the lines of `text` outside the tests that hold a `Receiver` in an `Option`, also
/// in a tuple or a `Box` there (`Option<(Receiver<…>, …)>`), or name a `Receiver` with a type
/// alias or a `use … as`, which an `Option` could then hold unseen.
fn bare_receivers_in(text: &str) -> Vec<usize> {
    let lines = code_lines(text);
    let mut found = Vec::new();
    for (i, line) in lines[..tests_start(&lines)].iter().enumerate() {
        let bare = line.replace("std::sync::", "").replace("mpsc::", "");
        let item = without_qualifiers(bare.trim_start());
        let is_alias = (item.starts_with("type ") && bare.contains("Receiver<"))
            || (item.starts_with("use ") && bare.contains("Receiver as "));
        // The receiver inside an `Option`, also in a tuple or a `Box` there.
        let in_option = (bare.match_indices("Option<")).any(|(at, option)| {
            let mut inside = &bare[at + option.len()..];
            while let Some(rest) = inside
                .strip_prefix("Box<")
                .or_else(|| inside.strip_prefix('('))
            {
                inside = rest;
            }
            inside.starts_with("Receiver<")
        });
        if in_option || is_alias {
            found.push(i + 1);
        }
    }
    found
}

/// The check above finds a `Receiver` in an `Option` however it is named, and a type alias of
/// one. It leaves a `Receiver` held for good and a `Receiving`.
#[test]
fn finds_a_bare_receiver() {
    let text = "struct A {\n    a: Option<Receiver<u8>>,\n    \
        b: Option<std::sync::mpsc::Receiver<u8>>,\n    c: Option<(mpsc::Receiver<u8>, u8)>,\n    \
        d: mpsc::Receiver<u8>,\n    e: Receiving<u8>,\n}\npub type Answer = Receiver<u8>;\n\
        use std::sync::mpsc::Receiver as Rx;\nstruct B {\n    f: Option<Box<Receiver<u8>>>,\n}\n";
    assert_eq!(bare_receivers_in(text), [2, 3, 4, 8, 9, 11]);
}

/// A thread that shows the busy pointer sends the window what it must still handle through an
/// `AnswerSender`, whose answers keep a busy pointer of their own until the window has handled
/// them. Before, such a thread let its pointer go right after it sent its answer. In that
/// moment the window saw no work running: a close dropped a Copy that still waited for its
/// folder's real path, and a test that waited for the work went on before it. A notice
/// (`NoticeSender`) is only shown, and the sidecar writer's thread holds the pointer itself, so
/// those are sent as they are. The threads listed here send parts of a list as they go, which
/// the window shows as they come, and nothing waits for the last part. A line here that no
/// longer matches fails too.
#[test]
fn answers_keep_the_busy_pointer() {
    const SEND_AS_THEY_GO: &[(&str, &str, &str)] = &[
        (
            "viewer/pan.rs",
            "pan_days",
            "the days read so far, drawn as they come",
        ),
        (
            "features/check.rs",
            "start_check",
            "each broken file, listed as it is found",
        ),
    ];
    let sources = rust_sources();
    let keeping = senders_that_keep_the_pointer(&sources);
    let mut found = Vec::new();
    // threads.rs holds the `AnswerSender` itself.
    for (file, text) in sources.iter().filter(|(f, _)| f != "support/threads.rs") {
        for (function, line) in answers_without_pointer(text, &keeping) {
            found.push((file.clone(), function, line));
        }
    }
    let is_listed = |file: &str, function: &str| {
        (SEND_AS_THEY_GO.iter()).any(|s| (s.0, s.1) == (file, function))
    };
    let wrong: Vec<String> = (found.iter())
        .filter(|(file, function, _)| !is_listed(file, function))
        .map(|(file, function, line)| format!("{file}:{line} {function}"))
        .collect();
    assert!(
        wrong.is_empty(),
        "a thread sends the window an answer, and its busy pointer goes before the window has \
         handled it (send it through an AnswerSender):\n{}",
        wrong.join("\n")
    );
    let unused: Vec<_> = (SEND_AS_THEY_GO.iter())
        .filter(|s| !(found.iter()).any(|f| (f.0.as_str(), f.1.as_str()) == (s.0, s.1)))
        .collect();
    assert!(
        unused.is_empty(),
        "named here but no longer there: {unused:?}"
    );
}

/// A thread started outside the tests shows the busy pointer while it runs (a `Working`, or an
/// `AnswerSender` for its answer), or says why it shows none, in a comment just above it that
/// starts with "No busy pointer:", or names the state that shows it ("Its busy pointer:").
/// `--remote … --wait` and the tests count on the busy count: work the user started and waits for,
/// without a pointer, let `--wait` answer before it was done. A look-up nobody waits for, a program
/// that may stay open for hours, a listener and a worker that last as long as the window say so
/// instead. A scoped thread (`s.spawn`) ends before its scope does, so the pointer of the thread
/// that runs the scope holds for it.
#[test]
fn threads_show_the_busy_pointer_or_say_why_not() {
    let mut wrong = Vec::new();
    for (file, text) in rust_sources() {
        let lines = threads_without_pointer(&text);
        wrong.extend(lines.iter().map(|line| format!("{file}:{line}")));
    }
    assert!(
        wrong.is_empty(),
        "a thread without the busy pointer (hold a Working, answer through an AnswerSender, or \
         say why not in a comment above it: \"No busy pointer:\" or \"Its busy pointer:\"):\n{}",
        wrong.join("\n")
    );
}

/// Returns the lines of `text` outside the tests that start a thread (`thread::spawn(`) in a
/// function that has made or been given no busy pointer up to that line (no `Working`,
/// `AnswerSender` or `answer_channel`), unless the comment lines just above the statement
/// say "No busy pointer:" or "Its busy pointer:".
fn threads_without_pointer(text: &str) -> Vec<usize> {
    let (code, comments) = (code_lines(text), comment_lines(text));
    let mut found = Vec::new();
    let mut has_pointer = false;
    for (i, line) in code[..tests_start(&code)].iter().enumerate() {
        if function_name(line).is_some() {
            has_pointer = false;
        }
        has_pointer |= ["Working", "AnswerSender", "answer_channel("]
            .iter()
            .any(|p| line.contains(p));
        if !line.contains("thread::spawn(") || has_pointer {
            continue;
        }
        // Up to the statement's first line, then up the comments and attributes above it.
        let mut first = i;
        while first > 0
            && !code[first - 1].trim().is_empty()
            && !code[first - 1].ends_with([';', '{', '}', ','])
        {
            first -= 1;
        }
        while first > 0
            && (code[first - 1].trim().is_empty() && !comments[first - 1].trim().is_empty()
                || code[first - 1].trim_start().starts_with("#["))
        {
            first -= 1;
        }
        let says_why = (first..=i).any(|j| comments[j].contains("busy pointer:"));
        if !says_why {
            found.push(i + 1);
        }
    }
    found
}

/// The check above finds a thread started without a pointer, also when another function made
/// one, and passes one with a `Working` made before it or given to its function, one that
/// answers through an `AnswerSender`, one whose comment above says why it needs none (also
/// behind an attribute), and a scoped thread.
#[test]
fn finds_a_thread_without_the_busy_pointer() {
    let text = "fn a(&self) {\n    let p = Working::new(&self.busy);\n    \
        std::thread::spawn(move || drop(p));\n}\nfn b() {\n    std::thread::spawn(f);\n}\n\
        fn c(p: Working) {\n    std::thread::spawn(move || drop(p));\n}\n\
        fn d(&self) {\n    let (tx, rx) = answer_channel(&self.busy);\n    \
        std::thread::spawn(f);\n}\nfn e() {\n    // No busy pointer: a listener.\n    \
        std::thread::spawn(f);\n    // A reaper.\n    std::thread::spawn(g);\n}\n\
        fn f() {\n    // No busy pointer: a reaper, which\n    // reaps.\n    \
        #[allow(clippy::disallowed_methods)] // a reason\n    \
        std::thread::spawn(move || child.wait());\n    std::thread::scope(|s| s.spawn(g));\n}\n";
    assert_eq!(threads_without_pointer(text), [6, 19]);
}

/// Returns the names, in `sources` outside the tests, of the fields, arguments and functions
/// that hold or give a sender whose message keeps the busy pointer, or needs none: an
/// `AnswerSender`, a `NoticeSender`, and the sidecar writer's `Sender<QueuedWrite>`.
fn senders_that_keep_the_pointer(sources: &[(String, String)]) -> HashSet<String> {
    const TYPES: [&str; 3] = ["AnswerSender<", "NoticeSender", "Sender<QueuedWrite>"];
    let mut names = HashSet::new();
    for (_, text) in sources {
        let lines = code_lines(text);
        let lines = &lines[..tests_start(&lines)];
        for (i, line) in lines.iter().enumerate() {
            // A function whose signature, up to its body, gives one back.
            if let Some(name) = function_name(line) {
                let signature: String = (lines[i..].iter())
                    .take_while(|l| !l.ends_with('{') && !l.ends_with(';'))
                    .chain(
                        lines[i..]
                            .iter()
                            .find(|l| l.ends_with('{') || l.ends_with(';')),
                    )
                    .map(String::as_str)
                    .collect();
                let gives = signature.split_once("->").map_or("", |(_, r)| r);
                if TYPES.iter().any(|t| without_paths(gives).contains(t)) {
                    names.insert(name.to_owned());
                }
                continue;
            }
            // A field or an argument of that type.
            for (at, _) in line.match_indices(": ") {
                if TYPES
                    .iter()
                    .any(|t| without_paths(&line[at + 2..]).starts_with(t))
                {
                    let before = &line[..at];
                    let name = before
                        .rsplit(|c: char| !c.is_alphanumeric() && c != '_')
                        .next()
                        .unwrap_or("");
                    names.insert(name.to_owned());
                }
            }
        }
    }
    names
}

/// Returns `text` without the paths before its names, as `NoticeSender` for
/// `crate::support::threads::NoticeSender`.
fn without_paths(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (k, part) in text.split("::").enumerate() {
        if k > 0 {
            let name_starts = out.trim_end_matches(|c: char| c.is_alphanumeric() || c == '_');
            out.truncate(name_starts.len());
        }
        out.push_str(part);
    }
    out
}

/// Returns the sends in `text` outside the tests, in a function that makes the busy pointer or
/// is given one (an argument of type `Working`), whose sender is not one of `keeping`, each as
/// (function, line). A sender is found by the `let` in the function that binds it, also in a
/// tuple, or else by its own name.
fn answers_without_pointer(text: &str, keeping: &HashSet<String>) -> Vec<(String, usize)> {
    let lines = code_lines(text);
    let end = tests_start(&lines);
    let mut found = Vec::new();
    let mut function = String::new();
    let mut has_pointer = false;
    // The names bound in the function, each with the expression it is bound to.
    let mut bound: Vec<(String, String)> = Vec::new();
    let mut sends: Vec<(String, usize)> = Vec::new();
    let mut statement: Option<String> = None;
    let words = |text: &str| -> Vec<String> {
        (text.split(|c: char| !c.is_alphanumeric() && c != '_'))
            .filter(|w| !w.is_empty())
            .map(str::to_owned)
            .collect()
    };
    for (i, line) in lines[..end]
        .iter()
        .map(String::as_str)
        .chain(["fn end"])
        .enumerate()
    {
        if let Some(name) = function_name(line) {
            if has_pointer {
                for (sender, at) in sends.drain(..) {
                    let binding = bound.iter().rev().find(|(n, _)| *n == sender);
                    let expression = binding.map_or(sender.clone(), |(_, e)| e.clone());
                    if !words(&expression).iter().any(|w| keeping.contains(w)) {
                        found.push((function.clone(), at));
                    }
                }
            }
            // A pointer that the caller made and passed in counts as one made here.
            let rest = &lines[i.min(end)..end];
            let signature: String = (rest.iter())
                .take_while(|l| !l.ends_with(['{', ';']))
                .chain(rest.iter().find(|l| l.ends_with(['{', ';'])))
                .map(String::as_str)
                .collect();
            let arguments = without_paths(signature.split("->").next().unwrap_or(""));
            has_pointer = words(&arguments).iter().any(|w| w == "Working");
            function = name.to_owned();
            (bound, sends, statement) = (Vec::new(), Vec::new(), None);
            continue;
        }
        has_pointer |= ["Working::new(&self.busy)", "Working::new(&app.busy)"]
            .iter()
            .any(|p| line.contains(p));
        for (at, _) in line.match_indices(".send(") {
            let before = line[..at].trim_end_matches("()");
            let sender = before
                .rsplit(|c: char| !c.is_alphanumeric() && c != '_')
                .next()
                .unwrap_or("");
            sends.push((sender.to_owned(), i + 1));
        }
        if statement.is_none() && line.trim_start().starts_with("let ") {
            statement = Some(String::new());
        }
        if let Some(text) = &mut statement {
            text.push_str(line.trim());
            text.push(' ');
            let depth = |open: char, close: char| {
                text.matches(open).count() as isize - text.matches(close).count() as isize
            };
            if text.trim_end().ends_with(';') && depth('(', ')') == 0 && depth('{', '}') == 0 {
                bound.extend(let_bindings(text));
                statement = None;
            }
        }
    }
    found
}

/// Returns the names that the `let` statement `text` binds, each with the expression bound to
/// it. In `let (a, b) = (x, y);` each name gets its own part, and otherwise each gets the whole
/// expression.
fn let_bindings(text: &str) -> Vec<(String, String)> {
    let Some((names, expression)) = text.trim().trim_start_matches("let ").split_once(" = ") else {
        return Vec::new();
    };
    let expression = expression.trim().trim_end_matches(';').trim();
    let top_level_parts = |inner: &str| -> Vec<String> {
        let (mut parts, mut depth, mut part) = (Vec::new(), 0, String::new());
        for c in inner.chars() {
            match c {
                '(' | '[' | '{' | '<' => depth += 1,
                ')' | ']' | '}' | '>' => depth -= 1,
                ',' if depth == 0 => {
                    parts.push(std::mem::take(&mut part));
                    continue;
                }
                _ => {}
            }
            part.push(c);
        }
        parts.push(part);
        parts
            .into_iter()
            .map(|p| p.trim().to_owned())
            .filter(|p| !p.is_empty())
            .collect()
    };
    let names: Vec<String> =
        top_level_parts(names.trim().trim_start_matches('(').trim_end_matches(')'))
            .iter()
            .map(|n| {
                n.trim_start_matches("mut ")
                    .split(':')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_owned()
            })
            .collect();
    let parts = match expression
        .strip_prefix('(')
        .and_then(|e| e.strip_suffix(')'))
    {
        Some(inner) => top_level_parts(inner),
        None => Vec::new(),
    };
    if names.len() > 1 && parts.len() == names.len() {
        names.into_iter().zip(parts).collect()
    } else {
        (names.into_iter())
            .map(|n| (n, expression.to_owned()))
            .collect()
    }
}

/// The check above finds an answer sent on a plain channel by a thread with the busy pointer,
/// also through a tuple's binding, and in a helper that its caller passed the pointer to. It
/// passes an `AnswerSender` and a notice, however they are bound, and a function without the
/// busy pointer.
#[test]
fn finds_an_answer_sent_without_its_pointer() {
    let text = "struct S {\n    report_tx: AnswerSender<u8>,\n}\n\
        fn a(&self) {\n    let (tx, rx) = mpsc::channel();\n    \
        let pointer = Working::new(&self.busy);\n    spawn(move || drop(tx.send(1)));\n}\n\
        fn b(&self) {\n    let (tx, ctx) = (self.notice_sender.clone(), c.clone());\n    \
        let (found, ctx) = (mpsc::channel().0, c.clone());\n    \
        let pointer = Working::new(&self.busy);\n    tx.send(s);\n    found.send(s);\n}\n\
        fn c(&self) {\n    let report_tx = self.report_tx.clone();\n    \
        let (t, r) = answer_channel(&self.busy);\n    \
        let p = Working::new(&self.busy);\n    report_tx.send(r);\n    t.send(1);\n}\n\
        fn d() {\n    let (tx, rx) = mpsc::channel();\n    tx.send(1);\n}\n\
        fn answer_channel<T>(\n    busy: &Arc<AtomicUsize>,\n) -> (AnswerSender<T>, R) {\n}\n\
        struct T {\n    notice_sender: crate::support::threads::NoticeSender,\n}\n\
        fn e(&self) {\n    let p = Working::new(&self.busy);\n    spawn_reader(p, tx);\n}\n\
        fn spawn_reader(\n    p: crate::support::threads::Working,\n    \
        tx: mpsc::Sender<u8>,\n) {\n    spawn(move || drop((tx.send(1), p)));\n}\n";
    let keeping = senders_that_keep_the_pointer(&[("x.rs".into(), text.into())]);
    let found = answers_without_pointer(text, &keeping);
    let found: Vec<(&str, usize)> = found.iter().map(|(f, l)| (f.as_str(), *l)).collect();
    assert_eq!(found, [("a", 7), ("b", 14), ("spawn_reader", 42)]);
}

/// Each name in backticks in AGENTS.md and docs/RELEASING.md that looks like code (a path with
/// `::`, or a word with `_`) is a word in the source, the tools, the workflows or Cargo.toml.
/// So a function, test or constant renamed or removed takes its name out of the rules too.
#[test]
fn the_rules_name_what_exists() {
    // Names that the rules give as examples, or that belong to another program.
    const NOT_IN_THE_CODE: [&str; 4] = [
        "selected_paths",   // the naming rule's example
        "read_sidecar",     // the naming rule's example
        "has_sidecar",      // the naming rule's example
        "FLATPAK_USER_DIR", // flatpak's own variable, in RELEASING.md
    ];
    fn read_tree(path: &Path, into: &mut String) {
        if path.is_dir() {
            for entry in std::fs::read_dir(path).unwrap().flatten() {
                read_tree(&entry.path(), into);
            }
        } else if let Ok(text) = std::fs::read_to_string(path) {
            into.push_str(&text);
            into.push('\n');
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut code = String::new();
    for part in ["src", "tools", ".github", "Cargo.toml", "clippy.toml"] {
        read_tree(&root.join(part), &mut code);
    }
    let words: std::collections::HashSet<&str> =
        (code.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')).collect();
    let mut wrong = Vec::new();
    for doc in ["AGENTS.md", "docs/RELEASING.md"] {
        let text = std::fs::read_to_string(root.join(doc)).unwrap();
        for quoted in text.split('`').skip(1).step_by(2) {
            let name = quoted.trim_end_matches("()");
            let is_path = name.split("::").all(|part| {
                part.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                    && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            });
            let last = name.rsplit("::").next().unwrap_or(name);
            let looks_like_code = name.contains("::") || last.contains('_');
            if is_path
                && looks_like_code
                && !words.contains(last)
                && !NOT_IN_THE_CODE.contains(&last)
            {
                wrong.push(format!("{doc}: {name}"));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "the rules name what is not in the code:\n{}",
        wrong.join("\n")
    );
}

/// The window's thread never waits for the disk: not for a user's file or folder, and not for
/// Glimt's own files either, because a home folder can be on a share too. On a share that
/// stopped answering (a NAS asleep, Wi-Fi gone) such a call waits for ever, and the window
/// hangs and cannot be closed. The window's thread is the code of `App`'s functions: the
/// `impl crate::App` blocks of every module, and main.rs's `impl App` and
/// `impl eframe::App for App`, because an `App` is never sent to another thread. The functions
/// of App's parts (`app_parts`) run there too, as App calls them. A call of `DISK_CALLS` there
/// is found, and so is a call of any other function of the crate that reaches one, however
/// many calls deep (`CallGraph`). The text of a call that hands work to another thread
/// (`spawn`, `write_later`, `run_in_background`, `write_sidecars`) is left out, in the window's
/// code and in the functions followed alike, because that work runs there. A call that must
/// stay is listed here with its function and the reason, and a line here that no longer
/// matches a call fails too.
#[test]
fn the_window_thread_never_waits_for_the_disk() {
    const WAITS_ON_PURPOSE: &[(&str, &str, &str, &str)] = &[
        (
            "files/file_ops.rs",
            "written_anew",
            "canonicalize(",
            "only files just written with a moved file's name; its shortcut comment",
        ),
        (
            "files/file_ops.rs",
            "written_anew",
            "fingerprint(",
            "only files just written with a moved file's name; its shortcut comment",
        ),
    ];
    let found = disk_calls_in_window_thread(&rust_sources(), &app_parts());
    let is_on_purpose = |file: &str, function: &str, call: &str| {
        (WAITS_ON_PURPOSE.iter()).any(|w| (w.0, w.1, w.2) == (file, function, call))
    };
    let wrong: Vec<String> = (found.iter())
        .filter(|(file, function, call, ..)| !is_on_purpose(file, function, call))
        .map(|(file, function, call, line, through)| {
            format!("{file}:{line} {function}: {call} ({through})")
        })
        .collect();
    assert!(
        wrong.is_empty(),
        "the window's thread waits for the disk (move it to a thread, as AGENTS.md says):\n{}",
        wrong.join("\n")
    );
    let unused: Vec<_> = (WAITS_ON_PURPOSE.iter())
        .filter(|w| {
            let is_w = |f: &(String, String, String, usize, String)| {
                (f.0.as_str(), f.1.as_str(), f.2.as_str()) == (w.0, w.1, w.2)
            };
            !found.iter().any(is_w)
        })
        .collect();
    assert!(
        unused.is_empty(),
        "named here but no longer there: {unused:?}"
    );
}

/// The window's thread never waits for a lock that the writer thread holds while it waits for
/// the disk, because a share that stopped answering would hang the window all the same. So no
/// statement of the marks that locks `KNOWN_STAMPS` (`known_stamps()`) waits for the disk, and
/// the window's own code, the methods of `Marks` outside the work they hand to the writer, does
/// not lock it at all.
#[test]
fn the_marks_lock_is_never_held_across_the_disk() {
    let file = "metadata/marks.rs";
    let graph = CallGraph::new(&rust_sources());
    let calls: Vec<Vec<Call>> = (graph.functions.iter())
        .map(|f| graph.calls_of(f))
        .collect();
    let ways = ways_to_the_disk(&calls);
    let to_the_disk: Vec<String> = (graph.functions.iter().zip(&ways))
        .filter(|(_, way)| way.is_some())
        .flat_map(|(f, _)| match f.owner {
            // A method is called on a value or through its type, never by its name alone.
            Some(_) => vec![format!(".{}(", f.name), format!("::{}(", f.name)],
            None => vec![format!("{}(", f.name)],
        })
        .chain(DISK_CALLS.iter().map(|c| (*c).to_owned()))
        .collect();
    let lines = &graph.lines[file];
    let mut wrong = Vec::new();
    for (i, line) in lines[..tests_start(lines)].iter().enumerate() {
        let statement = statement_text(lines, i);
        let waits = (to_the_disk.iter()).find(|c| mentions_call(&statement, c));
        if let Some(call) = waits.filter(|_| line.contains("known_stamps()")) {
            wrong.push(format!("{file}:{} locks and then waits for {call}", i + 1));
        }
    }
    for f in (graph.functions.iter()).filter(|f| f.owner.as_deref() == Some("Marks")) {
        let mut handed_over = 0;
        for i in f.lines.clone() {
            if code_of_this_thread(&lines[i], &mut handed_over).contains("known_stamps()") {
                wrong.push(format!(
                    "{file}:{} the window locks it in {}",
                    i + 1,
                    f.name
                ));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "the marks' lock is held across the disk:\n{}",
        wrong.join("\n")
    );
}

/// Returns whether `text` calls `call` (`name(`, or a call of `DISK_CALLS`), not merely a
/// function whose name ends in it.
fn mentions_call(text: &str, call: &str) -> bool {
    let starts_a_name = call.starts_with(is_word_char);
    (text.match_indices(call)).any(|(at, _)| !starts_a_name || !text[..at].ends_with(is_word_char))
}

/// Returns the calls in `sources` that wait for the disk in the window's thread, each as (file,
/// function, call, line, the calls it goes through to the disk): in the functions of `App` and
/// of its `parts` (each as (file, struct)), the calls of `DISK_CALLS` and of the crate's
/// functions that reach one. A call of another function of the window's thread is not counted
/// where it is made, because that function's own calls are.
fn disk_calls_in_window_thread(
    sources: &[(String, String)],
    parts: &[(String, String)],
) -> Vec<(String, String, String, usize, String)> {
    let graph = CallGraph::new(sources);
    let is_window = |f: &Function| {
        f.owner.as_deref().is_some_and(|owner| {
            owner == "App" || (parts.iter()).any(|(file, part)| *file == f.file && part == owner)
        })
    };
    // A call of a function of the window's thread leads to the disk only through that
    // function's own calls, which are found where it makes them. So it is left out, also from
    // the ways through other functions to the disk.
    let calls: Vec<Vec<Call>> = (graph.functions.iter())
        .map(|f| {
            let mut calls = graph.calls_of(f);
            calls.retain(|c| (c.callee).is_none_or(|g| !is_window(&graph.functions[g])));
            calls
        })
        .collect();
    let ways = ways_to_the_disk(&calls);
    let mut found = Vec::new();
    for (f, calls_of_f) in graph.functions.iter().zip(&calls) {
        if !is_window(f) {
            continue;
        }
        for call in calls_of_f {
            let waits = (call.callee).is_none_or(|g| ways[g].is_some());
            if !waits {
                continue;
            }
            // The way on, from the function called to a call of `DISK_CALLS`.
            let mut through = Vec::new();
            let mut next = call.callee;
            while let Some(g) = next.filter(|_| through.len() < 20) {
                let on = &calls[g][ways[g].unwrap_or(0)];
                through.push(format!("{} {}", graph.functions[g].file, on.text));
                next = on.callee;
            }
            let (file, function, text) = (f.file.clone(), f.name.clone(), call.text.clone());
            found.push((file, function, text, call.line, through.join(" → ")));
        }
    }
    found.dedup();
    found
}

/// Returns, for each function of `calls` (the calls of each function, by its index), which of
/// its calls reaches a call of `DISK_CALLS`, being one or through the functions it calls, or
/// `None` when none does.
fn ways_to_the_disk(calls: &[Vec<Call>]) -> Vec<Option<usize>> {
    let mut ways: Vec<Option<usize>> = (calls.iter())
        .map(|c| c.iter().position(|call| call.callee.is_none()))
        .collect();
    loop {
        let mut changed = false;
        for (i, calls) in calls.iter().enumerate() {
            if ways[i].is_none() {
                ways[i] = (calls.iter()).position(|c| c.callee.is_some_and(|g| ways[g].is_some()));
                changed |= ways[i].is_some();
            }
        }
        if !changed {
            return ways;
        }
    }
}

/// Returns the functions of `file`, whose `code_lines` are `lines`, outside its tests: the free
/// ones and the methods of its `impl` and `trait` blocks. A function inside another is part of
/// that one.
fn functions_in(file: &str, lines: &[String]) -> Vec<Function> {
    let end = tests_start(lines);
    let (mut functions, mut owner, mut i) = (Vec::new(), None, 0);
    while i < end {
        let line = &lines[i];
        if indent(line) == 0 && line.ends_with('{') {
            owner = block_owner(without_qualifiers(line));
        }
        let name = function_name(line).filter(|_| indent(line) <= 4 && !line.ends_with(';'));
        let Some(name) = name else {
            i += 1;
            continue;
        };
        let braces = |brace: char| line.matches(brace).count();
        let is_one_line = line.ends_with('}') && braces('{') == braces('}');
        let last = match is_one_line {
            true => i,
            false => block_end(lines, i).unwrap_or(i),
        };
        functions.push(Function {
            file: file.to_owned(),
            owner: owner.clone().filter(|_| indent(line) == 4),
            name: name.to_owned(),
            lines: i..=last,
        });
        i = last + 1;
    }
    functions
}

/// Returns the type whose `impl` or `trait` block the top-level `item` starts, such as `Folder`
/// for `impl Default for Folder {` and `App` for `impl crate::App {`, or `None` for another item.
fn block_owner(item: &str) -> Option<String> {
    if let Some(name) = item.strip_prefix("trait ") {
        return name.split([' ', '<', ':', '{']).next().map(str::to_owned);
    }
    let rest = item.strip_prefix("impl")?;
    // The generic parameters of `impl<T>` come before the type.
    let rest = match rest.strip_prefix('<') {
        Some(generic) => generic.split_once("> ")?.1,
        None => rest,
    };
    let rest = rest.trim().trim_end_matches('{').trim();
    let rest = rest.rsplit(" for ").next()?.split(" where").next()?;
    let name = rest.split('<').next()?.rsplit("::").next()?.trim();
    Some(name.to_owned())
}

/// Returns the type of each field of the crate's structs with named fields, by (struct, field),
/// as `part_held_by` reads it: without the `Option` around it and its generic arguments.
fn field_types(
    sources: &[(String, String)],
) -> std::collections::HashMap<(String, String), String> {
    let mut types = std::collections::HashMap::new();
    for (_, text) in sources {
        let lines = code_lines(text);
        let mut held_by: Option<String> = None;
        for line in &lines[..tests_start(&lines)] {
            if let Some(name) = struct_name(line) {
                held_by = Some(name.to_owned());
                continue;
            }
            if line.starts_with('}') {
                held_by = None;
            }
            let field = without_qualifiers(line.trim_start());
            if let Some(owner) = &held_by
                && indent(line) == 4
                && let Some((name, field_type)) = field.split_once(": ")
            {
                let held = part_held_by(field_type).1.to_owned();
                types.insert((owner.clone(), name.to_owned()), held);
            }
        }
    }
    types
}

/// Returns the paths in `code` that may name a function (`name`, `module::name`,
/// `Type::name`), each with where it starts.
fn paths_in(code: &str) -> Vec<(usize, String)> {
    let mut paths = Vec::new();
    let mut chars = code.char_indices().peekable();
    let mut previous = ' ';
    while let Some((at, c)) = chars.next() {
        if !(c.is_alphabetic() || c == '_') || is_word_char(previous) {
            previous = c;
            continue;
        }
        let mut path = String::from(c);
        while let Some(&(_, next)) = chars.peek() {
            if !(is_word_char(next) || next == ':') {
                break;
            }
            path.push(next);
            chars.next();
        }
        previous = path.chars().last().unwrap_or(c);
        paths.push((at, path.trim_end_matches(':').to_owned()));
    }
    paths
}

/// Returns whether the function whose code is `text`, and whose lines up to its body are
/// `signature`, binds `name` itself: as a parameter, a `let` or a closure's argument. A call of
/// that name is then a call of the local, not of a function.
fn binds_locally(text: &str, signature: &str, name: &str) -> bool {
    let bound = |code: &str, binds: &dyn Fn(&str, &str) -> bool| {
        code.match_indices(name).any(|(at, _)| {
            let (before, after) = (&code[..at], &code[at + name.len()..]);
            let is_word = !before.ends_with(is_word_char) && !after.starts_with(is_word_char);
            is_word && binds(before, after)
        })
    };
    let is_parameter =
        |before: &str, after: &str| after.starts_with(": ") && !before.ends_with(':');
    let is_local = |before: &str, after: &str| {
        before.ends_with("let ")
            || before.ends_with("let mut ")
            || before.ends_with('|')
            || after.starts_with('|')
    };
    bound(signature, &is_parameter) || bound(text, &is_local)
}

/// Returns the receiver that the lines of `lines` before line `at` end in, when line `at` goes
/// on with a method: the names and fields at the end of the line before (`self`, `.folder`),
/// and of the lines before that while they go on with a field too. Returns what ends in a call
/// or an index as nothing, as its type is not found.
fn receiver_before(lines: &[String], at: usize) -> String {
    let mut receiver = String::new();
    for line in lines[..at].iter().rev() {
        let line = line.trim();
        let start = (line.rfind(|c: char| !(is_word_char(c) || c == '.'))).map_or(0, |i| i + 1);
        receiver.insert_str(0, &line[start..]);
        if start > 0 || !line.starts_with('.') {
            break;
        }
    }
    receiver
}

/// Returns the type that a function writes for its own `word`: a parameter in its
/// `signature` (`ui: &mut egui::Ui`) or a `let` in its `text` (`let tool: Tool`). The type is
/// given without its references, path and generics, and an `Arc`, `Rc` or `Box` gives the type
/// it holds, whose methods it passes on. Returns `None` when no type is written for `word`.
fn written_type(text: &str, signature: &str, word: &str) -> Option<String> {
    if word.is_empty() || !word.chars().all(is_word_char) {
        return None;
    }
    let pattern = format!("{word}: ");
    let after_parameter = (signature.match_indices(&pattern))
        .filter(|(at, _)| !signature[..*at].ends_with(is_word_char))
        .map(|(at, _)| &signature[at + pattern.len()..]);
    let after_let = (text.match_indices(&pattern))
        .filter(|(at, _)| {
            let before = &text[..*at];
            before.ends_with("let ") || before.ends_with("let mut ")
        })
        .map(|(at, _)| &text[at + pattern.len()..]);
    after_parameter.chain(after_let).find_map(|mut written| {
        loop {
            written = written.trim_start_matches(['&', ' ']);
            written = written.strip_prefix("mut ").unwrap_or(written);
            let end =
                (written.find(|c: char| !(is_word_char(c) || c == ':'))).unwrap_or(written.len());
            let name = written[..end].rsplit("::").next().unwrap_or("");
            match written[end..].strip_prefix('<') {
                Some(held) if ["Arc", "Rc", "Box"].contains(&name) => written = held,
                _ => {
                    break name
                        .starts_with(char::is_uppercase)
                        .then(|| name.to_owned());
                }
            }
        }
    })
}

/// Returns the part of `line` that runs in the thread the line runs in: all of it, the part
/// before a call that hands work to another thread, or nothing while the parentheses of such a
/// call on an earlier line are still open. `handed_over` counts how deep they still go.
fn code_of_this_thread<'l>(line: &'l str, handed_over: &mut usize) -> &'l str {
    let (mut own, mut handed) = ("", line);
    if *handed_over == 0 {
        match HANDED_TO_A_THREAD.iter().filter_map(|c| line.find(c)).min() {
            None => return line,
            Some(at) => (own, handed) = line.split_at(at),
        }
    }
    for c in handed.chars() {
        match c {
            '(' => *handed_over += 1,
            ')' => *handed_over = handed_over.saturating_sub(1),
            _ => {}
        }
    }
    own
}

/// The check above finds a look at the disk or a change on it in the functions of `App` and of
/// its parts, also through the crate's functions that reach one however deep, and also through
/// a function given as a value. It finds them before a thread is started on the same line, and
/// after the thread's call ends. It passes the work handed to a thread, also inside a function
/// followed; a function of the window's thread, whose own calls are found instead; a local or a
/// parameter of the same name as a function; a method of another type than the one written for
/// its receiver (a parameter's, a `let`'s, held in an `Arc`, also on the lines before); a
/// function without `self` called as a method; a free function; a comment and a string. A
/// method called on a value whose type is not written (a loop's variable, a closure's argument,
/// what a call returns) is followed to every method of that name.
#[test]
fn finds_a_disk_call_in_the_window_thread() {
    let a = "impl crate::App {\n    fn a(&self) {\n        std::fs::metadata(p);\n    }\n    \
        fn b(&self) {\n        std::thread::spawn(move || {\n            p.exists();\n        \
        });\n        let d = helper(&p);\n    }\n    fn c(&self) {\n        \
        write_later(move || drop(helper(&p)));\n        // p.is_dir()\n        \
        let s = \".is_file()\";\n        let x = q.is_dir(); std::thread::spawn(f);\n    \
        }\n    fn f(&mut self, look: L) {\n        self.part.d();\n        b::deep(&p);\n        \
        Other::look(p);\n        let helper = |p| p;\n        helper(p);\n        look(p);\n        \
        q.map(in_thread);\n        refuse(&p, look_deeper);\n    }\n    \
        fn g(&self, ui: &mut egui::Ui) {\n        ui.label(p);\n        modified(p);\n    }\n    \
        fn h(&self) {\n        self.part.get().label();\n        \
        for x in items {\n            x.label();\n        }\n        \
        items.map(|t| t.label());\n    }\n    \
        fn i(&self, other: &Other, part: Arc<Part>) {\n        other.label();\n        other.each(f);\n        \
        part.label();\n        let tool: Thing = make();\n        tool.label();\n        part\n            .label();\n        \
        text.look(p);\n    }\n}\nfn helper(p: &Path) -> bool {\n    \
        look_deeper(p)\n}\nfn look_deeper(p: &Path) -> bool {\n    p.is_dir()\n}\nfn in_thread() {\n    \
        std::thread::spawn(move || p.exists());\n}\nfn refuse(p: &Path, look: L) -> bool {\n    \
        look(p)\n}\nfn modified(p: &Path) {\n    p.metadata();\n}\nfn look(p: &Path) {\n    \
        p.is_dir();\n}\nstruct App {\n    part: Part,\n}\nimpl Part {\n    fn d(&self) {\n        \
        p.metadata();\n    }\n}\nimpl Default for Part {\n    fn default() -> Self {\n        \
        p.is_symlink();\n    }\n}\nimpl Other {\n    fn look(p: &Path) {\n        \
        p.is_dir();\n    }\n    pub(crate) fn label(&self) {\n        p.is_dir();\n    }\n    \
        fn each<F: Fn(&Path) -> bool>(&self, f: F) {\n        p.is_dir();\n    }\n}\n";
    // A chain three calls deep, through the functions of two other modules, to the disk. The
    // way is told by where each call on it is made.
    let b = "pub fn deep(p: &Path) {\n    middle(p);\n}\nfn middle(p: &Path) {\n    \
        crate::c::inner(p);\n}\n";
    let c = "pub fn inner(p: &Path) {\n    std::fs::read_dir(p);\n}\n";
    let sources = [
        ("a.rs".into(), a.into()),
        ("b.rs".into(), b.into()),
        ("c.rs".into(), c.into()),
    ];
    let found = disk_calls_in_window_thread(&sources, &[("a.rs".into(), "Part".into())]);
    let deep = found.iter().find(|f| f.2 == "deep(").map(|f| f.4.as_str());
    assert_eq!(deep, Some("b.rs middle( → b.rs inner( → c.rs read_dir("));
    let found: Vec<(&str, &str, &str, usize)> = (found.iter())
        .map(|(file, f, c, l, _)| (file.as_str(), f.as_str(), c.as_str(), *l))
        .collect();
    assert_eq!(
        found,
        [
            ("a.rs", "a", "fs::metadata(", 3),
            ("a.rs", "b", "helper(", 9),
            ("a.rs", "c", ".is_dir()", 15),
            ("a.rs", "f", "deep(", 19),
            ("a.rs", "f", "look(", 20),
            ("a.rs", "f", "look_deeper", 25),
            ("a.rs", "g", "modified(", 29),
            ("a.rs", "h", "label(", 32),
            ("a.rs", "h", "label(", 34),
            ("a.rs", "h", "label(", 36),
            ("a.rs", "i", "label(", 39),
            ("a.rs", "i", "each(", 40),
            ("a.rs", "d", ".metadata()", 72),
            ("a.rs", "default", ".is_symlink()", 77),
        ]
    );
}

/// `the_window_thread_never_waits_for_the_disk` also finds a wait for a program that a
/// `Watchdog` limits, which takes minutes, through a function that reaches no other call it
/// looks for. Before, only `run_limited`, `output_of` and `short_output_of` were found.
#[test]
fn finds_a_wait_for_a_program_in_the_window_thread() {
    let a = "impl crate::App {\n    fn a(&self) {\n        send_to_printer(p);\n    }\n    \
        fn b(&self) {\n        mount(p);\n    }\n}\nfn send_to_printer(p: &Path) {\n    \
        child.wait_with_output();\n}\nfn mount(p: &Path) {\n    child.wait();\n}\n";
    let found = disk_calls_in_window_thread(&[("a.rs".into(), a.into())], &[]);
    let found: Vec<(&str, &str)> = (found.iter())
        .map(|(_, f, c, ..)| (f.as_str(), c.as_str()))
        .collect();
    assert_eq!(found, [("a", "send_to_printer("), ("b", "mount(")]);
}

/// An XMP property is found only as a top-level property of a Description
/// (`xmp::top_level_properties`), never by a search of the packet for its name. Such a search
/// also finds the property inside another one, such as the keywords, the rating or the
/// orientation of another document in xmpMM:Pantry. The readers took those for the photo's own,
/// and the editors took them out or changed them.
#[test]
fn xmp_properties_are_found_at_the_top_level() {
    // This file names such searches in its own tests.
    let sources: Vec<(String, String)> = (rust_sources().into_iter())
        .filter(|(file, _)| file != "support/source_rules.rs")
        .collect();
    let wrong = searches_for_xmp_properties(&sources);
    assert!(
        wrong.is_empty(),
        "find an XMP property with xmp::top_level_properties, not by a search for its name:\n{}",
        wrong.join("\n")
    );
}

/// The check above finds a search for a property by its name in each way it can be written: a
/// method of `str` or of a slice, a window compared, `memchr::memmem`, a crate's own search
/// such as `find_bytes` or `ForwardSearch`, with the name as a literal, a `let`, a constant or
/// `format!`, and also when a function passes its own parameter on as what is searched for and
/// a caller gives it the name. The two searches before 0.82.0, `set_xmp_orientation`'s and
/// `xmp_without_property`'s, are found as they were written then. A search for a Description,
/// a list item or a comment's end passes, and so does a comparison with a name that the walk
/// found.
#[test]
fn finds_a_search_for_an_xmp_property() {
    let exif = "pub fn set_xmp_orientation(xmp: &mut [u8], value: u8) {\n    \
        let key = b\"tiff:Orientation\";\n    let mut from = 0;\n    \
        while let Some(at) = xmp[from..].windows(key.len()).position(|w| w == key) {\n        \
        from = at;\n    }\n}\n";
    let save = "pub(crate) fn xmp_without_property(xmp: &[u8], name: &[u8]) -> Vec<u8> {\n    \
        let end_tag = [&b\"</\"[..], name, b\">\"].concat();\n    \
        let [mut names, mut gts] = [\n        ForwardSearch::new(xmp, name),\n        \
        ForwardSearch::new(xmp, b\">\"),\n    ];\n    \
        let mut opens = ForwardSearch::new(xmp, b\"<!--\");\n    Vec::new()\n}\n";
    let xmp = "pub(crate) fn without_property(xmp: &str, tag: &str) -> String {\n    \
        let out = crate::images::save::xmp_without_property(xmp.as_bytes(), tag.as_bytes());\n    \
        String::from_utf8(out).unwrap_or_else(|_| xmp.to_owned())\n}\n\
        fn set_keywords(xmp: &str) -> String {\n    without_property(xmp, \"dc:subject\")\n}\n";
    let other = "const DESCRIPTION: &[u8] = b\"<rdf:Description\";\n\
        const RATING: &str = \"xmp:Rating=\";\n\
        fn find_bytes(hay: &[u8], what: &[u8]) -> Option<usize> {\n    \
        hay.windows(what.len()).position(|w| w == what)\n}\n\
        fn a(x: &str, tag: &str, found: &[u8]) {\n    x.contains(\"<dc:subject\");\n    \
        x.find(&format!(\"<{tag}\"));\n    x.find(\"<rdf:Description\");\n    \
        find_bytes(x.as_bytes(), b\"-->\");\n    \
        ForwardSearch::new(x.as_bytes(), DESCRIPTION);\n    \
        x.find(|c: char| c == ':');\n    \
        find_bytes(x.as_bytes(), b\"<dc:subject\");\n    \
        memchr::memmem::find(x.as_bytes(), b\"lr:hierarchicalSubject\");\n    \
        x.find(RATING);\n    \
        let named = found == b\"tiff:Orientation\";\n    \
        x.match_indices(\"xmp:Rating\");\n}\n";
    let sources = [
        ("exif.rs", exif),
        ("save.rs", save),
        ("xmp.rs", xmp),
        ("a.rs", other),
    ];
    let sources = sources.map(|(file, text)| (file.to_owned(), text.to_owned()));
    assert_eq!(
        searches_for_xmp_properties(&sources),
        [
            "a.rs:13 through a.rs:4",
            "a.rs:14",
            "a.rs:15",
            "a.rs:17",
            "a.rs:7",
            "a.rs:8",
            "exif.rs:4",
            "xmp.rs:6 through xmp.rs:2 through save.rs:4",
        ]
    );
}

/// Each method of `str` and of slices that looks for a pattern is a search, one spelling at a
/// time, with the name searched for in its place among the arguments. A search for an RDF
/// name with the same method passes. Before, `split`, `starts_with`, `strip_prefix`,
/// `replace`, `matches` and their kin got past the check.
#[test]
fn finds_a_search_for_an_xmp_property_in_every_spelling() {
    let spellings = [
        "contains(N)",
        "find(N)",
        "rfind(N)",
        "match_indices(N)",
        "rmatch_indices(N)",
        "matches(N)",
        "rmatches(N)",
        "split_once(N)",
        "rsplit_once(N)",
        "split(N)",
        "rsplit(N)",
        "splitn(2, N)",
        "rsplitn(2, N)",
        "split_terminator(N)",
        "rsplit_terminator(N)",
        "split_inclusive(N)",
        "starts_with(N)",
        "ends_with(N)",
        "strip_prefix(N)",
        "strip_suffix(N)",
        "trim_start_matches(N)",
        "trim_end_matches(N)",
        "trim_matches(N)",
        "replace(N, \"\")",
        "replacen(N, \"\", 1)",
    ];
    for spelling in spellings {
        let search = |name: &str| {
            let call = spelling.replace('N', &format!("\"{name}\""));
            let text = format!("fn a(x: &str) {{\n    x.{call};\n}}\n");
            searches_for_xmp_properties(&[("a.rs".into(), text)])
        };
        assert_eq!(search("<dc:subject"), ["a.rs:2"], "{spelling}");
        assert!(search("<rdf:li").is_empty(), "{spelling}");
        // A URN, such as the name of an ISO gain map's segment, is no property's name.
        assert!(
            search("urn:iso:std:iso:ts:21496:-1").is_empty(),
            "{spelling}"
        );
    }
    // v0.81.1's search for the items of a motion photo, an editor, a hand-written walk and a
    // name bound by a loop.
    let a = "fn jpeg_trailer_is_cut(xmp: &str) {\n    \
        let items = xmp.split(\"<Container:Item\").skip(2);\n}\n\
        fn edit(xmp: &str) -> String {\n    xmp.replace(\"xmp:Rating=\\\"3\\\"\", \"\")\n}\n\
        fn walk(xmp: &str, i: usize) -> bool {\n    let rest = &xmp[i..];\n    \
        rest.starts_with(\"<dc:subject\")\n}\n\
        fn turn(xmp: &[u8]) {\n    for key in [b\"<rdf:li\", b\"tiff:Orientation\"] {\n        \
        memchr::memmem::find(xmp, key);\n    }\n}\n";
    assert_eq!(
        searches_for_xmp_properties(&[("a.rs".into(), a.into())]),
        ["a.rs:13", "a.rs:2", "a.rs:5", "a.rs:9"]
    );
}

/// Returns the places outside the tests of `sources` where a packet is searched for an XMP
/// property by its name, as "file:line", in order. A search for a name of the RDF or XML
/// namespace, such as a Description or a list item, is no search for a property. A function
/// whose search looks for its own parameter is a search too, so a caller that gives it a
/// property's name is found, at the place of the search that the name reaches.
fn searches_for_xmp_properties(sources: &[(String, String)]) -> Vec<String> {
    let mut searches: Vec<(String, usize, Option<String>)> = (TEXT_SEARCHES.iter())
        .map(|&(call, at)| (call.to_owned(), at, None))
        .collect();
    let mut wrong = std::collections::BTreeSet::new();
    loop {
        let known = searches.len();
        for (file, text) in sources {
            let (raw, code) = (text.lines().collect::<Vec<_>>(), code_lines(text));
            for f in functions_in(file, &code) {
                let found = searches_in(&f, &raw, &code, &searches);
                for (needle, line, through) in found {
                    let where_found = match through {
                        Some(search) => format!("{file}:{line} through {search}"),
                        None => format!("{file}:{line}"),
                    };
                    match searched_for(&needle, &f, &raw, &code, 0) {
                        Searched::Property => _ = wrong.insert(where_found),
                        Searched::Parameter(at) => {
                            let call = format!("{}(", f.name);
                            if !searches.iter().any(|(c, a, _)| *c == call && *a == at) {
                                searches.push((call, at, Some(where_found)));
                            }
                        }
                        Searched::Other => {}
                    }
                }
            }
        }
        if searches.len() == known {
            return wrong.into_iter().collect();
        }
    }
}

/// Returns the searches in function `f` of a file whose lines are `raw` and whose `code_lines`
/// are `code`, each as what it looks for (as written), its line counted from 1, and the place of
/// the search it reaches when it calls a function of the crate that searches for its parameter.
/// `searches` are the calls that search, as in `TEXT_SEARCHES`, each with that place.
fn searches_in(
    f: &Function,
    raw: &[&str],
    code: &[String],
    searches: &[(String, usize, Option<String>)],
) -> Vec<(String, usize, Option<String>)> {
    let (raw_text, code_text) = function_texts(f, raw, code);
    let line_of = |at: usize| f.lines.start() + 1 + code_text[..at].matches('\n').count();
    let mut found = Vec::new();
    for (call, argument, through) in searches {
        for (at, _) in code_text.match_indices(call.as_str()) {
            let before = code_text[..at].chars().next_back().unwrap_or(' ');
            let is_whole_name = call.starts_with('.') || !is_word_char(before);
            if !is_whole_name || code_text[..at].ends_with("fn ") {
                continue;
            }
            let arguments = call_arguments(&code_text, at + call.len());
            if let Some(span) = arguments.get(*argument) {
                let needle = raw_text[span.clone()].iter().collect();
                found.push((needle, line_of(at), through.clone()));
            }
        }
    }
    for (at, needle) in window_needles(&code_text, &raw_text) {
        found.push((needle, line_of(at), None));
    }
    found
}

/// Returns the text of function `f` twice, as characters: as written in `raw`, and as code only
/// from `code`, with a character of the one at each place of the other.
fn function_texts(f: &Function, raw: &[&str], code: &[String]) -> (Vec<char>, String) {
    let (mut raw_text, mut code_text) = (Vec::new(), String::new());
    for i in f.lines.clone() {
        let written: Vec<char> = raw.get(i).unwrap_or(&"").chars().collect();
        // The code line lost its blanks at the end, and a literal's letter of several bytes is
        // one blank in it, so it is laid out again by the characters of the line.
        let mut code_line: Vec<char> = code[i].chars().collect();
        code_line.resize(written.len(), ' ');
        raw_text.extend(written);
        raw_text.push('\n');
        code_text.extend(
            code_line
                .into_iter()
                .map(|c| if c.is_ascii() { c } else { ' ' }),
        );
        code_text.push('\n');
    }
    (raw_text, code_text)
}

/// Returns where each argument of the call whose arguments start at `from` in `code` is, up to
/// the ')' that closes the call. An argument may be blank in `code`, as a literal is blanked
/// there. Returns nothing when the call does not close.
fn call_arguments(code: &str, from: usize) -> Vec<std::ops::Range<usize>> {
    let (mut out, mut depth, mut start) = (Vec::new(), 0usize, from);
    for (i, c) in code[from..].char_indices() {
        let at = from + i;
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' if depth > 0 => depth -= 1,
            ')' => {
                out.push(start..at);
                return out;
            }
            ',' if depth == 0 => {
                out.push(start..at);
                start = at + 1;
            }
            _ => {}
        }
    }
    Vec::new()
}

/// Returns each window compared in `code`, the code of a function whose text as written is
/// `raw` (`function_texts`), with where it is and what the window is compared with, as written:
/// the `name` of `windows(n).position(|w| w == name)` or `.any(|w| name == w)`.
fn window_needles(code: &str, raw: &[char]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (at, call) in code.match_indices(".windows(") {
        let Some(size) = call_arguments(code, at + call.len()).pop() else {
            continue;
        };
        let rest = &code[size.end + 1..];
        let Some(open) = rest
            .find('(')
            .filter(|&i| rest[..i].trim_start().starts_with('.'))
        else {
            continue;
        };
        let from = size.end + 1 + open + 1;
        let Some(closure) = call_arguments(code, from).pop() else {
            continue;
        };
        let body = &code[closure.clone()];
        // The closure's argument is between its two bars, and the comparison after them.
        let Some(argument) = body.trim_start().strip_prefix('|') else {
            continue;
        };
        let opened = body.len() - argument.len();
        let Some(bar) = argument.find('|').map(|i| opened + i) else {
            continue;
        };
        let Some(equals) = body[bar..].find("==").map(|i| bar + i) else {
            continue;
        };
        let variable = body[opened..bar].trim();
        let sides = [closure.start + bar + 1..closure.start + equals, {
            closure.start + equals + 2..closure.end
        }];
        let text = |r: &std::ops::Range<usize>| raw[r.clone()].iter().collect::<String>();
        let other = sides
            .iter()
            .map(text)
            .find(|side| side.trim().trim_start_matches(['*', '&']) != variable);
        out.extend(other.map(|needle| (at, needle.trim().to_owned())));
    }
    out
}

/// Returns what `needle`, the text searched for as written in function `f`, is: a property's
/// name, one of `f`'s parameters, or something else. A name bound by a `let` or a `for` loop in
/// `f` or a constant of its file (`raw` and `code`) is read as what it is bound to, and
/// `format!`, an array of parts and an array looped through by their parts, `depth` levels deep
/// at most.
fn searched_for(needle: &str, f: &Function, raw: &[&str], code: &[String], depth: u8) -> Searched {
    let mut needle = needle.trim().trim_start_matches(['&', '*']).trim();
    for suffix in [
        ".as_bytes()",
        ".as_str()",
        ".as_slice()",
        ".as_ref()",
        "[..]",
        ".to_vec()",
        ".iter()",
        ".into_iter()",
    ] {
        needle = needle.strip_suffix(suffix).unwrap_or(needle);
    }
    if depth > 4 || needle.starts_with('|') || needle.starts_with("move |") {
        return Searched::Other;
    }
    let parts: Vec<Searched> = if let Some(template) = needle.strip_prefix("format!(") {
        let template = quoted_text(template).unwrap_or_default();
        let placeholders = template
            .split('{')
            .skip(1)
            .filter_map(|p| p.split('}').next());
        let named = placeholders.map(|p| searched_for(p, f, raw, code, depth + 1));
        let starts_a_tag = template.starts_with("<{") || template.starts_with("</{");
        std::iter::once(match starts_a_tag || names_a_property(&template) {
            true => Searched::Property,
            false => Searched::Other,
        })
        .chain(named)
        .collect()
    } else if let Some(text) = quoted_text(needle).filter(|_| needle.ends_with('"')) {
        vec![match names_a_property(&text) {
            true => Searched::Property,
            false => Searched::Other,
        }]
    } else if let Some(inside) = needle
        .strip_suffix(".concat()")
        .and_then(|n| n.strip_prefix('['))
    {
        let inside = inside.trim_end_matches(']');
        (inside
            .split(',')
            .map(|p| searched_for(p, f, raw, code, depth + 1)))
        .collect()
    } else if let Some(inside) = needle.strip_prefix('[').and_then(|n| n.strip_suffix(']')) {
        // An array that a loop goes through, as in `for name in ["tiff:Orientation"]`.
        (inside
            .split(',')
            .map(|p| searched_for(p, f, raw, code, depth + 1)))
        .collect()
    } else if !needle.is_empty() && needle.chars().all(is_word_char) {
        vec![bound_value(needle, f, raw, code, depth)]
    } else {
        Vec::new()
    };
    let property = parts.contains(&Searched::Property);
    let parameter = parts
        .into_iter()
        .find(|p| matches!(p, Searched::Parameter(_)));
    match (property, parameter) {
        (true, _) => Searched::Property,
        (false, Some(parameter)) => parameter,
        (false, None) => Searched::Other,
    }
}

/// Returns what the name `word` is in function `f` of a file whose lines are `raw` and whose
/// `code_lines` are `code`: one of its parameters, or what a `let` or a `for` loop in it or a
/// constant of the file binds it to (`searched_for`).
fn bound_value(word: &str, f: &Function, raw: &[&str], code: &[String], depth: u8) -> Searched {
    if let Some(at) = parameters(f, code).iter().position(|p| p == word) {
        return Searched::Parameter(at);
    }
    let in_function = (f.lines.clone()).filter_map(|i| raw.get(i).copied());
    // A name bound by a `for` loop is each item of what the loop goes through, so an array
    // of names is read item by item.
    let looped = |l: &str| {
        let (name, rest) = l.trim_start().strip_prefix("for ")?.split_once(" in ")?;
        let items = rest.trim_end().strip_suffix('{')?.trim();
        Some((
            name.trim().trim_start_matches("mut ").to_owned(),
            items.to_owned(),
        ))
    };
    let bound = (in_function.filter_map(|l| match l.trim_start().starts_with("let ") {
        true => Some(let_bindings(l)),
        false => looped(l).map(|binding| vec![binding]),
    }))
    .flatten()
    .find(|(name, _)| name == word)
    .map(|(_, value)| value);
    let constant = format!("const {word}: ");
    let constant = || {
        let line = raw.iter().find(|l| l.trim_start().starts_with(&constant))?;
        Some(line.split_once(" = ")?.1.trim_end_matches(';').to_owned())
    };
    match bound.or_else(constant) {
        Some(value) => searched_for(&value, f, raw, code, depth + 1),
        None => Searched::Other,
    }
}

/// Returns the names of the parameters of function `f`, whose file's `code_lines` are `code`,
/// in order and without `self`.
fn parameters(f: &Function, code: &[String]) -> Vec<String> {
    let lines = &code[f.lines.clone()];
    let opens = lines.iter().position(|l| l.ends_with('{')).unwrap_or(0);
    let signature = lines[..=opens].join(" ");
    let Some(at) = signature.find(&format!("fn {}", f.name)) else {
        return Vec::new();
    };
    // The parameters start at the first '(' after the generics, and are parted by the commas
    // outside every bracket, also the angle brackets of a type (`HashMap<K, V>`).
    let (mut depth, mut previous) = (0usize, ' ');
    let (mut names, mut parameter, mut started) = (Vec::new(), String::new(), false);
    for c in signature[at + 3 + f.name.len()..].chars() {
        let is_arrow = c == '>' && previous == '-';
        previous = c;
        match c {
            '(' if depth == 0 && !started => {
                started = true;
                continue;
            }
            '<' | '(' | '[' => depth += 1,
            '>' if is_arrow => {}
            ')' if depth == 0 => break,
            '>' | ')' | ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 && started => {
                names.push(std::mem::take(&mut parameter));
                continue;
            }
            _ => {}
        }
        if started {
            parameter.push(c);
        }
    }
    names.push(parameter);
    (names.iter())
        .filter_map(|p| {
            let name = p.trim().trim_start_matches('&').trim_start_matches("mut ");
            let name = name.split(':').next()?.trim();
            (!name.is_empty() && name != "self").then(|| name.to_owned())
        })
        .collect()
}

/// Returns the text between the first quote of `text` and the next quote after it, as written,
/// or `None` when `text` has no quote.
fn quoted_text(text: &str) -> Option<String> {
    let (_, rest) = text.split_once('"')?;
    Some(rest.split('"').next().unwrap_or(rest).to_owned())
}

/// Returns whether `text` starts with the name of an XMP property, after a "<" or "</" when it
/// has one: a prefix, a ':' and a name that starts with a letter. The names of the RDF and XML
/// namespaces (rdf:li, xml:lang, xmlns:dc) are no property's. Nor is a name with a second ':'
/// in it, such as a URN ("urn:iso:std:…"), as an XML name has one ':' at most.
fn names_a_property(text: &str) -> bool {
    let text = text
        .strip_prefix("</")
        .or(text.strip_prefix('<'))
        .unwrap_or(text);
    let Some((prefix, local)) = text.split_once(':') else {
        return false;
    };
    let is_prefix = !prefix.is_empty() && prefix.chars().all(|c| is_word_char(c) || c == '-');
    let is_name_char = |c: char| is_word_char(c) || matches!(c, '-' | '.' | ':');
    let local_name = local.split(|c: char| !is_name_char(c)).next().unwrap_or("");
    is_prefix
        && local.starts_with(|c: char| c.is_ascii_alphabetic())
        && !local_name.contains(':')
        && !["rdf", "xml", "xmlns"].contains(&prefix)
}

/// Every walk over an XMP packet passes over its comments, CDATA sections and processing
/// instructions through `xmp::not_element_length`. A walk that does not finds a Description, a
/// property or a namespace declaration inside one, which XML does not see. A namespace declared
/// in a comment once made the star writer write xmp:Rating with its prefix bound to nothing.
#[test]
fn packet_walks_pass_over_markup_that_is_no_element() {
    let sources: Vec<(String, String)> = (rust_sources().into_iter())
        .filter(|(file, _)| file != "support/source_rules.rs")
        .collect();
    let wrong = walks_without_markup_skip(&sources);
    assert!(
        wrong.is_empty(),
        "pass over comments, CDATA and processing instructions with xmp::not_element_length, \
         or say in the doc comment how the text given has none (`not_element_length`):\n{}",
        wrong.join("\n")
    );
}

/// The check above finds a walk from one '<' to the next, and a search for namespace
/// declarations, that neither calls `not_element_length` nor names it in its doc comment. It
/// finds `namespace_declarations` as it was written before this check.
#[test]
fn finds_a_walk_that_does_not_pass_over_markup() {
    let a = "/// The namespaces.\nfn namespace_declarations(xmp: &str) -> Vec<(&str, &str)> {\n    \
        for (at, _) in xmp.match_indices(\"xmlns:\") {\n        out.push(at);\n    }\n}\n\
        fn walk(x: &[u8]) {\n    let at = x.iter().position(|&c| c == b'<');\n}\n\
        fn text(x: &str) {\n    x.find('<');\n}\n\
        fn skips(x: &[u8]) {\n    x.iter().position(|&c| c == b'<');\n    \
        not_element_length(x);\n}\n\
        /// Text outside markup (`not_element_length`).\nfn given(x: &str) {\n    \
        x.match_indices(\"xmlns:\");\n}\n\
        fn tag(name: &[u8]) -> bool {\n    name.starts_with(b\"xmlns:\")\n}\n";
    let found = walks_without_markup_skip(&[("a.rs".into(), a.into())]);
    assert_eq!(
        found,
        [
            "a.rs:2 namespace_declarations",
            "a.rs:7 walk",
            "a.rs:10 text"
        ]
    );
}

/// Returns the functions outside the tests of `sources` that walk a packet from one '<' to the
/// next or search it for namespace declarations, without `not_element_length` in their body or
/// in the doc comment above them, as "file:line name".
fn walks_without_markup_skip(sources: &[(String, String)]) -> Vec<String> {
    const WALKS: [&str; 5] = [
        "c == b'<'",
        "find('<')",
        "match_indices('<')",
        "match_indices(\"xmlns",
        "find(\"xmlns",
    ];
    let names_the_skip = |l: &&str| l.contains("not_element_length");
    let mut wrong = Vec::new();
    for (file, text) in sources {
        let (raw, code) = (text.lines().collect::<Vec<_>>(), code_lines(text));
        for f in functions_in(file, &code) {
            let body: Vec<&str> = (f.lines.clone())
                .filter_map(|i| raw.get(i).copied())
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect();
            if !body
                .iter()
                .any(|l| WALKS.iter().any(|walk| l.contains(walk)))
            {
                continue;
            }
            let is_doc = |l: &&&str| {
                let l = l.trim_start();
                l.starts_with("///") || l.starts_with("#[")
            };
            let mut doc = raw[..*f.lines.start()].iter().rev().take_while(is_doc);
            if !(body.iter().any(names_the_skip) || doc.any(names_the_skip)) {
                wrong.push(format!("{file}:{} {}", f.lines.start() + 1, f.name));
            }
        }
    }
    wrong
}

/// A pool's workers judge each job by its file, never by its place in the list, because a
/// listing moves the files while the jobs wait: each part of a tree read with Include Subfolders
/// is a new list. The decoders and then the thumbnail workers judged by place, and on a slow
/// share the parts came faster than the jobs were served, so the jobs were skipped and asked for
/// again until the tree was read. In a function that starts a pool, no line about its jobs or
/// what its workers share (a channel, an `Arc` or `Mutex`, a closure), with the crate's type
/// aliases written out, names a `usize`.
#[test]
fn worker_pools_judge_jobs_by_their_files() {
    let sources = rust_sources();
    let aliases = type_aliases(&sources);
    let mut wrong = Vec::new();
    for (file, text) in &sources {
        wrong.extend(pools_judged_by_place(file, text, &aliases));
    }
    assert!(
        wrong.is_empty(),
        "send a pool's workers files, not places in the list:\n{}",
        wrong.join("\n")
    );
}

/// The check above finds a place in a job, in a channel and in what the workers share through a
/// type alias, but not a count of workers.
#[test]
fn finds_a_pool_judged_by_place() {
    let text = "type Shown = Arc<Mutex<Vec<usize>>>;\n\
        fn a(shown: &Shown) {\n    spawn_worker_pool(n, rx, |_| {}, \
        move |(i, path): (usize, PathBuf)| go(i));\n}\n\
        fn b(shown: &Shown) {\n    spawn_worker_pool(n, rx, |_| {}, go);\n}\n\
        fn c() -> mpsc::Sender<(usize, PathBuf)> {\n    \
        spawn_worker_pool(n, rx, |_| {}, go);\n}\n\
        fn d(\n    workers: usize,\n    shown: &Arc<Mutex<Vec<PathBuf>>>,\n) {\n    \
        spawn_worker_pool(n, rx, |_| {}, move |path: PathBuf| go(path));\n}\n\
        fn e(shown: &Shown) {\n    go(shown);\n}\n";
    let sources = [("x.rs".to_owned(), text.to_owned())];
    let aliases = type_aliases(&sources);
    assert_eq!(
        pools_judged_by_place("x.rs", text, &aliases),
        ["x.rs: a", "x.rs: b", "x.rs: c"]
    );
}

/// Returns the crate's type aliases in `sources`, each name with what it stands for.
fn type_aliases(sources: &[(String, String)]) -> Vec<(String, String)> {
    let mut aliases = Vec::new();
    for (_, text) in sources {
        for line in code_lines(text) {
            let Some(rest) = without_qualifiers(line.trim_start()).strip_prefix("type ") else {
                continue;
            };
            if let Some((name, stands_for)) = rest.split_once('=') {
                let name = name.split('<').next().unwrap_or(name).trim();
                aliases.push((name.to_owned(), stands_for.to_owned()));
            }
        }
    }
    aliases
}

/// Returns the functions in `text`, the source of `file`, that start a worker pool with a place
/// in the list (a `usize`) in a line about its jobs: a channel, an `Arc` or `Mutex`, or a
/// closure, with the type aliases in `aliases` written out.
fn pools_judged_by_place(file: &str, text: &str, aliases: &[(String, String)]) -> Vec<String> {
    let lines = code_lines(text);
    let end = tests_start(&lines);
    let has_word = |line: &str, word: &str| {
        (line.match_indices(word)).any(|(at, _)| {
            !line[..at].ends_with(is_word_char)
                && !line[at + word.len()..].starts_with(is_word_char)
        })
    };
    let (mut function, mut starts_pool, mut by_place) = (String::new(), false, false);
    let mut wrong = Vec::new();
    // A last line that starts no function ends the last one, so it is checked too.
    let last = "fn end".to_owned();
    for line in lines[..end].iter().chain([&last]) {
        if let Some(name) = function_name(line) {
            if starts_pool && by_place {
                wrong.push(format!("{file}: {function}"));
            }
            (function, starts_pool, by_place) = (name.to_owned(), false, false);
        }
        starts_pool |= line.contains("spawn_worker_pool(");
        let mut written_out = line.clone();
        for (name, stands_for) in aliases {
            if has_word(line, name) {
                written_out.push_str(stands_for);
            }
        }
        let about_jobs = ["mpsc", "Arc<", "Mutex<", "|"]
            .iter()
            .any(|w| written_out.contains(w));
        by_place |= about_jobs && has_word(&written_out, "usize");
    }
    wrong
}
