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
    "paint_other",   // paint_compared
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
    "own_dir",         // is_usable_top_trash
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
    "is_unlocked",       // take_lock_if_free
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
    ("files/journal.rs", "free"),           // take_lock_if_free
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

/// The calls that look at or change the disk: the standard library's, and Glimt's own functions
/// that do, the writers of file_io among them. Each is found anywhere in a line, also as a
/// method (`path.read_dir()`).
const DISK_CALLS: &[&str] = &[
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
    "real_dir(",
    "real_path(",
    "by_real_path(",
    "collections::load(",
    "edit_open_collection(",
    "read_limited(",
    "read_text(",
    "read_head(",
    "open_regular(",
    "image_reader(",
    "fingerprint(",
    "same_file(",
    "file_system_type_or_test",
    "rename_never_over(",
    "put_file(",
    "replace_name(",
    "replace_name_with(",
    "replace_file(",
    "replace_existing(",
    "write_beside(",
    "write_beside_without_folder_sync(",
    "create_file(",
    "create_temp_beside(",
    "edit_shared(",
    "remove_shared(",
    "file_lock(",
    "sync_folder(",
    "sync_folder_of(",
    "trash::trash(",
    "collections::edit(",
    "write_atomic(",
];

/// Glimt's own small functions that wrap a call of `DISK_CALLS`, by a name that is common
/// enough to be a method too. Each is found only as a call of a function of that name
/// (`calls_function`).
const DISK_HELPERS: &[&str] = &["modified(", "saved_collections(", "open_own_folder("];

/// The calls that hand work to another thread, whose text `DISK_CALLS` are not looked for in.
const HANDED_TO_A_THREAD: &[&str] = &[
    "spawn(",
    "write_later(",
    "write_later_in_user_folders(",
    "run_in_background(",
    "write_sidecars(",
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
#[test]
fn tests_use_temporary_names_of_their_own() {
    let mut seen: std::collections::HashMap<String, String> = Default::default();
    let mut wrong = Vec::new();
    for (name, text) in sources().iter().filter(|(n, _)| n.ends_with(".rs")) {
        for (at, _) in text.match_indices("\"glimt-test-") {
            let rest = &text[at + 1..];
            let Some(end) = rest.find('"') else { continue };
            let literal = &rest[..end];
            if let Some(first) = seen.insert(literal.to_owned(), name.clone()) {
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
/// (`resume_unwind`) or tells it.
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
/// thread's `join()` whose error is dropped with `.ok()`, `.unwrap_or_default()`,
/// `.unwrap_or(…)`, `let _ =`, `_ =` or `drop(…)`, also on the next line.
fn panics_caught_in(text: &str) -> Vec<usize> {
    const SWALLOWED: [&str; 3] = [".ok()", ".unwrap_or_default()", ".unwrap_or("];
    let lines = code_lines(text);
    let end = tests_start(&lines);
    let mut found = Vec::new();
    for (i, line) in lines[..end].iter().enumerate() {
        let after_join = line.split_once(".join()").map(|(_, after)| after.trim());
        let next = lines.get(i + 1).map_or("", |l| l.trim());
        let swallowed = after_join.is_some_and(|after| {
            let after = if after.is_empty() { next } else { after };
            let statement = line.trim_start();
            SWALLOWED.iter().any(|s| after.starts_with(s))
                || ["let _ =", "_ =", "drop("]
                    .iter()
                    .any(|s| statement.starts_with(s))
        });
        if line.contains("catch_unwind") || swallowed {
            found.push(i + 1);
        }
    }
    found
}

/// The check above finds `catch_unwind` and a join whose error is dropped, on its line or the
/// next. It leaves a join that passes the panic on, one that unwraps in the tests, and a
/// `join` of text.
#[test]
fn finds_a_panic_caught() {
    let text = "fn a() {\n    std::panic::catch_unwind(f);\n    \
        workers.flat_map(|w| w.join().unwrap_or_default());\n    let _ = feed.join();\n    \
        handle\n        .join()\n        .ok();\n    \
        p.join().unwrap_or_else(|e| std::panic::resume_unwind(e));\n    \
        let text = parts.join(\", \");\n    drop(h.join());\n}\n\
        mod tests {\n    fn t() {\n        h.join().ok();\n    }\n}\n";
    assert_eq!(panics_caught_in(text), [2, 3, 4, 6, 10]);
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
    const WRITERS: &[(&str, &[(&str, &str)])] = &[
        ("USER_KEYS.write()", &[("ui/actions.rs", "use_user_keys")]),
        (
            "MAX_ANIMATION_BYTES.store(",
            &[("images/decode.rs", "set_animation_limit")],
        ),
        (
            "VIEW_FILTER.store(",
            &[
                ("images/colour.rs", "set_up_colour"),
                ("ui/menus.rs", "set_view_filter"),
            ],
        ),
        (
            "COLOR_MANAGEMENT.store(",
            &[
                ("images/colour.rs", "set_up_colour"),
                ("ui/menus.rs", "set_color_management"),
            ],
        ),
    ];
    let wrong = outside_their_homes(WRITERS);
    assert!(
        wrong.is_empty(),
        "a copy of a setting written by hand:\n{}",
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
        "icons",
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
    assert!(
        wrong.is_empty(),
        "set through their setters instead:\n{}",
        wrong.join("\n")
    );
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
}

/// Returns each place outside the tests in `text` where a setting is written, as the function,
/// the line's number and the setting. A setting is written when it is assigned (`=`, `+=` and
/// the like) or taken as `&mut`, as `self.config.X` or through a name that the function bound
/// to `&mut self.config`.
fn config_writes(text: &str) -> Vec<(String, usize, String)> {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    const ASSIGN: &[&str] = &[
        "=", "+=", "-=", "*=", "/=", "%=", "|=", "&=", "^=", "<<=", ">>=",
    ];
    let lines = code_lines(text);
    let mut function = String::new();
    let mut prefixes = Vec::new();
    let mut writes = Vec::new();
    for (i, line) in lines[..tests_start(&lines)].iter().enumerate() {
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
        if let Some(rest) = bound.filter(|r| r.ends_with("= &mut self.config;")) {
            let rest = rest.strip_prefix("mut ").unwrap_or(rest);
            let name: String = rest.chars().take_while(|&c| ident(c)).collect();
            prefixes.push(format!("{name}."));
        }
        for prefix in &prefixes {
            for (at, _) in line.match_indices(prefix.as_str()) {
                if line[..at].ends_with(|c: char| ident(c) || c == '.') {
                    continue;
                }
                let rest = &line[at + prefix.len()..];
                let name: String = rest.chars().take_while(|&c| ident(c)).collect();
                let after = rest[name.len()..].trim_start();
                let assigned = ASSIGN.iter().any(|op| after.starts_with(op))
                    && !after.starts_with("==")
                    && !after.starts_with("=>");
                if !name.is_empty() && (assigned || line[..at].ends_with("&mut ")) {
                    writes.push((function.clone(), i + 1, name));
                }
            }
        }
    }
    writes
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
/// calls by the new name would not hold `text`.
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
                && match is_use {
                    true => after.starts_with(" as "),
                    false => after.starts_with([')', ',']),
                }
        })
}

/// Returns whether `c` can be part of a name.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// A call is found also as a function value and as renamed by a `use`, but not in a `use` that
/// keeps its name, nor in a longer name.
#[test]
fn finds_a_call_by_another_spelling() {
    const HOMES: &[(&str, &[(&str, &str)])] = &[("replace_file(", &[("a.rs", "home")])];
    let text = "use crate::files::file_io::replace_file as write_own;\n\
        use crate::files::file_io::{replace_file, write_atomic};\nfn a() {\n    \
        paths.iter().map(file_io::replace_file);\n    try_all(replace_file, &paths);\n    \
        try_all(own_replace_file, &paths);\n    replace_files(p);\n}\n";
    let wrong = outside_homes_in("a.rs", text, HOMES);
    let lines: Vec<&str> = (wrong.iter())
        .map(|w| w.split(' ').next().unwrap())
        .collect();
    assert_eq!(lines, ["a.rs:1", "a.rs:4", "a.rs:5"]);
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
                ("files/config.rs", "save"),
                ("metadata/marks.rs", "write"),
                ("model/folder.rs", "remember_recent"),
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
        // The keys read before the shortcuts: Enter in the grid, a zoomed image's arrows, and
        // ↑ and ↓ for the icon grid's rows (`set_row_step`).
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
        let setting = [".thumbnails", ".icons"].iter().any(|s| {
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
/// could call them directly. Only the Pan view does, for the cells of a day's images.
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
        ("row_clicked(", &[("ui/file_list.rs", "file_list")]),
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
        ("metadata/xmp.rs", "follow_one"),         // a sidecar after its file
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
        let syncs = ["sync_folder_of(", "sync_folder(", "write_beside("];
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
        if !clippy.contains(&format!("path = \"{call}\"")) {
            wrong.push(format!("clippy.toml: {call} is no longer refused"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
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

/// Returns the lines of the statements of `text` outside the tests that drop the result of one
/// of file_io's writers, with `let _ =` (or `let _: T =`), `_ =`, `.ok();`,
/// `.unwrap_or_default();`, `.unwrap_or(());`, `drop(…)`, `let _name =` where `_name` is not
/// used again, or `if let Ok(…) =` with no `else`.
fn dropped_writes_in(text: &str) -> Vec<usize> {
    const WRITERS: [&str; 14] = [
        // collections::edit and edit_open_collection, which write through replace_name.
        "edit(",
        "edit_open_collection(",
        "sync_folder_of(",
        "sync_folder(",
        "write_beside_without_folder_sync(",
        "rename_never_over(",
        "put_file(",
        "edit_shared(",
        "replace_file(",
        "replace_existing(",
        "replace_name(",
        "replace_name_with(",
        "write_beside(",
        "create_file(",
    ];
    const KEYWORDS: [&str; 7] = ["if", "while", "match", "return", "let", "else", "for"];
    let lines = code_lines(text);
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
            let words = statement.trim();
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
            if (dropped && WRITERS.iter().any(is_writer))
                || WRITERS.iter().any(|w| is_dropped(words, w))
            {
                found.push(first_line);
            }
            statement.clear();
        }
    }
    found
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

/// The check above finds a dropped write in each of its forms, also over several lines and in a
/// match arm, and leaves a write that is handled alone, and a bound name that is used.
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
}

/// A background thread's answer is held in a `threads::Receiving`, never in a bare
/// `Option<Receiver>`, because `Receiving` tells once when the thread is gone, and a bare one
/// waits for an answer that never comes.
#[test]
fn answers_are_received_through_receiving() {
    let mut wrong = Vec::new();
    for (file, text) in rust_sources() {
        if !file.ends_with("support/threads.rs") {
            wrong.extend(
                bare_receivers_in(&text)
                    .iter()
                    .map(|line| format!("{file}:{line}")),
            );
        }
    }
    assert!(
        wrong.is_empty(),
        "a bare Option<Receiver> (hold it in a threads::Receiving):\n{}",
        wrong.join("\n")
    );
}

/// Returns the lines of `text` outside the tests that hold a `Receiver` in an `Option`, also
/// in a tuple there (`Option<(Receiver<…>, …)>`), or name a `Receiver` with a type alias, which
/// an `Option` could then hold unseen.
fn bare_receivers_in(text: &str) -> Vec<usize> {
    let lines = code_lines(text);
    let mut found = Vec::new();
    for (i, line) in lines[..tests_start(&lines)].iter().enumerate() {
        let bare = line.replace("std::sync::", "").replace("mpsc::", "");
        let is_alias = without_qualifiers(bare.trim_start()).starts_with("type ")
            && bare.contains("Receiver<");
        if bare.contains("Option<Receiver<") || bare.contains("Option<(Receiver<") || is_alias {
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
        d: mpsc::Receiver<u8>,\n    e: Receiving<u8>,\n}\npub type Answer = Receiver<u8>;\n";
    assert_eq!(bare_receivers_in(text), [2, 3, 4, 8]);
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

/// Returns the sends in `text` outside the tests, in a function that makes the busy pointer,
/// whose sender is not one of `keeping`, each as (function, line). A sender is found by the
/// `let` in the function that binds it, also in a tuple, or else by its own name.
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
            (function, has_pointer) = (name.to_owned(), false);
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
/// also through a tuple's binding. It passes an `AnswerSender` and a notice, however they are
/// bound, and a function without the busy pointer.
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
        struct T {\n    notice_sender: crate::support::threads::NoticeSender,\n}\n";
    let keeping = senders_that_keep_the_pointer(&[("x.rs".into(), text.into())]);
    let found = answers_without_pointer(text, &keeping);
    let found: Vec<(&str, usize)> = found.iter().map(|(f, l)| (f.as_str(), *l)).collect();
    assert_eq!(found, [("a", 7), ("b", 14)]);
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

/// The window's thread never waits for the disk of a user's file or folder. On a share that
/// stopped answering (a NAS asleep, Wi-Fi gone) such a call waits for ever, and the window hangs
/// and cannot be closed. The window's thread is the code of `App`'s methods: the
/// `impl crate::App` blocks of every module, and main.rs's `impl App` and
/// `impl eframe::App for App`, because an `App` is never sent to another thread. The methods
/// of App's parts (`app_parts`) run there too, as App calls them. Inside them, the text of a
/// call that hands work to another thread (`spawn`, `write_later`, `run_in_background`,
/// `write_sidecars`) is left out, because that work runs there. A function of Glimt's own
/// that looks at the disk is found by its name too. A call that must stay is listed here with
/// its function and the reason, and a line here that no longer matches a call fails too.
#[test]
fn the_window_thread_never_waits_for_the_disk() {
    const WAITS_ON_PURPOSE: &[(&str, &str, &str, &str)] = &[
        (
            "main.rs",
            "with_config",
            "read_text(",
            "Glimt's own window state, read once at start, before the window opens",
        ),
        (
            "ui/dialogs.rs",
            "dialog_start",
            ".is_dir()",
            "Glimt's own ICC folder under $HOME, once when the file dialog opens",
        ),
        (
            "files/file_ops.rs",
            "rename_files_of",
            "file_system_type_or_test",
            "only a new name with one of : ? * \" < > | \\, once before its job starts",
        ),
        (
            "files/file_ops.rs",
            "rename_folder",
            "file_system_type_or_test",
            "only a new name with one of : ? * \" < > | \\, once before its thread starts",
        ),
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
        (
            "metadata/marks.rs",
            "load",
            "read_limited(",
            "Glimt's own marks file, read once at start, before the window opens",
        ),
        (
            "model/folder.rs",
            "new",
            "read_limited(",
            "Glimt's own Open Recent and sorted folders, read once at start",
        ),
        (
            "ui/info_panel.rs",
            "new",
            "read_limited(",
            "Glimt's own keyword tree, read once at start",
        ),
    ];
    let mut found: Vec<(String, String, String, usize)> = Vec::new();
    let app_parts = app_parts();
    let sources = rust_sources();
    for (file, text) in &sources {
        let parts: Vec<&str> = (app_parts.iter())
            .filter(|(f, _)| f == file)
            .map(|(_, part)| part.as_str())
            .collect();
        for (function, call, line) in disk_calls_in_window_thread(text, &parts) {
            found.push((file.clone(), function, call, line));
        }
    }
    let is_on_purpose = |file: &str, function: &str, call: &str| {
        (WAITS_ON_PURPOSE.iter()).any(|w| (w.0, w.1, w.2) == (file, function, call))
    };
    let wrong: Vec<String> = (found.iter())
        .filter(|(file, function, call, _)| !is_on_purpose(file, function, call))
        .map(|(file, function, call, line)| format!("{file}:{line} {function}: {call}"))
        .collect();
    assert!(
        wrong.is_empty(),
        "the window's thread waits for the disk (move it to a thread, as AGENTS.md says):\n{}",
        wrong.join("\n")
    );
    let unused: Vec<_> = (WAITS_ON_PURPOSE.iter())
        .filter(|w| {
            let is_w = |f: &(String, String, String, usize)| f.0 == w.0 && f.1 == w.1 && f.2 == w.2;
            !found.iter().any(is_w)
        })
        .collect();
    assert!(
        unused.is_empty(),
        "named here but no longer there: {unused:?}"
    );
}

/// Returns the calls in `text` that look at the disk in the window's thread, as the methods of
/// `App` and of its `parts` in `text` outside the work they hand to other threads, each as
/// (function, call, line).
fn disk_calls_in_window_thread(text: &str, parts: &[&str]) -> Vec<(String, String, usize)> {
    let lines = code_lines(text);
    let end = tests_start(&lines);
    let mut found = Vec::new();
    let (mut in_app, mut function) = (false, String::new());
    // How deep the parentheses of a call that hands work to another thread still go.
    let mut handed_over = 0usize;
    for (i, line) in lines[..end].iter().enumerate() {
        let item = without_qualifiers(line);
        let is_part = |part: &&str| {
            item.starts_with("impl")
                && (item.ends_with(&format!(" {part} {{"))
                    || item.ends_with(&format!(">{part} {{")))
        };
        if [
            "impl crate::App {",
            "impl App {",
            "impl eframe::App for App {",
        ]
        .contains(&item)
            || parts.iter().any(is_part)
        {
            in_app = true;
            continue;
        }
        if line.starts_with('}') {
            in_app = false;
        }
        if !in_app {
            continue;
        }
        if let Some(name) = function_name(line) {
            function = name.to_owned();
        }
        let call = disk_call_in_line(line, &mut handed_over);
        found.extend(call.map(|c| (function.clone(), c, i + 1)));
    }
    found
}

/// Returns the first call of `DISK_CALLS` or `DISK_HELPERS` that `line` makes outside the work
/// it hands to another thread, and counts in `handed_over` how deep the parentheses of such a
/// hand-over still go after it.
fn disk_call_in_line(line: &str, handed_over: &mut usize) -> Option<String> {
    let mut code = line;
    let mut found = None;
    if *handed_over == 0 {
        let at = HANDED_TO_A_THREAD.iter().filter_map(|c| line.find(c)).min();
        // What stands before the hand-over is still the window's.
        let before = &line[..at.unwrap_or(line.len())];
        found = (DISK_CALLS.iter().find(|c| before.contains(*c)))
            .map(|c| (*c).to_owned())
            .or_else(|| {
                DISK_HELPERS
                    .iter()
                    .find(|c| calls_function(before, c))
                    .map(|c| (*c).to_owned())
            });
        match at {
            Some(at) => code = &line[at..],
            None => return found,
        }
    }
    for c in code.chars() {
        match c {
            '(' => *handed_over += 1,
            ')' => *handed_over = handed_over.saturating_sub(1),
            _ => {}
        }
    }
    found
}

/// Returns whether `code` calls the function `call` (`name(`): no other name, method's dot or
/// `fn ` stands right before it, so the helper `modified(` is not found in `meta.modified()`,
/// `last_modified(` or a method of that name.
fn calls_function(code: &str, call: &str) -> bool {
    code.match_indices(call).any(|(at, _)| {
        let before = &code[..at];
        !before.ends_with(|c: char| c.is_alphanumeric() || c == '_' || c == '.')
            && !before.ends_with("fn ")
    })
}

/// The check above finds a look at the disk or a change on it in `App`'s methods, also through
/// Glimt's own functions and helpers, before a thread is started on the same line, and after
/// the thread's call ends. It passes the work handed to a thread, a free function, a method of
/// the same name as a helper, a comment and a string.
#[test]
fn finds_a_disk_call_in_the_window_thread() {
    let text = "impl crate::App {\n    fn a(&self) {\n        std::fs::metadata(p);\n    }\n    \
        fn b(&self) {\n        std::thread::spawn(move || {\n            p.exists();\n        \
        });\n        let d = real_dir(&p);\n    }\n    fn c(&self) {\n        \
        write_later(move || drop(p.canonicalize()));\n        // p.is_dir()\n        \
        let s = \".is_file()\";\n        let x = q.is_dir(); std::thread::spawn(f);\n    \
        }\n}\nfn free() {\n    std::fs::read_dir(p);\n}\nimpl Part {\n    fn d(&self) {\n        \
        fingerprint(p);\n    }\n}\nimpl Default for Part {\n    fn default() -> Self {\n        \
        p.metadata();\n    }\n}\nimpl Other {\n    fn e(&self) {\n        p.is_dir();\n    }\n}\n\
        impl crate::App {\n    fn f(&mut self) {\n        let _ = file_io::rename_never_over(a, b);\n        \
        trash::trash(&paths);\n        modified(p);\n        m.modified();\n        \
        remove_file(p);\n        fn modified(&self) {}\n        p.is_symlink();\n    }\n}\n";
    let found = disk_calls_in_window_thread(text, &["Part"]);
    let found: Vec<(&str, &str, usize)> = (found.iter())
        .map(|(f, c, l)| (f.as_str(), c.as_str(), *l))
        .collect();
    assert_eq!(
        found,
        [
            ("a", "fs::metadata(", 3),
            ("b", "real_dir(", 9),
            ("c", ".is_dir()", 15),
            ("d", "fingerprint(", 23),
            ("default", ".metadata()", 28),
            ("f", "rename_never_over(", 38),
            ("f", "trash::trash(", 39),
            ("f", "modified(", 40),
            ("f", "remove_file(", 42),
            ("modified", ".is_symlink()", 44),
        ]
    );
}
