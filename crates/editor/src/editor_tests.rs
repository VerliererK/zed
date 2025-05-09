use super::*;
use crate::{
    scroll::scroll_amount::ScrollAmount,
    test::{
        assert_text_with_selections, build_editor, editor_lsp_test_context::EditorLspTestContext,
        editor_test_context::EditorTestContext, select_ranges,
    },
    JoinLines,
};
use futures::StreamExt;
use gpui::{
    div, BackgroundExecutor, SemanticVersion, TestAppContext, UpdateGlobal, VisualTestContext,
    WindowBounds, WindowOptions,
};
use indoc::indoc;
use language::{
    language_settings::{
        AllLanguageSettings, AllLanguageSettingsContent, LanguageSettingsContent, PrettierSettings,
    },
    BracketPairConfig,
    Capability::ReadWrite,
    FakeLspAdapter, IndentGuide, LanguageConfig, LanguageConfigOverride, LanguageMatcher,
    LanguageName, Override, ParsedMarkdown, Point,
};
use language_settings::{Formatter, FormatterList, IndentGuideSettings};
use multi_buffer::MultiBufferIndentGuide;
use parking_lot::Mutex;
use project::{buffer_store::BufferChangeSet, FakeFs};
use project::{
    lsp_command::SIGNATURE_HELP_HIGHLIGHT_CURRENT,
    project_settings::{LspSettings, ProjectSettings},
};
use serde_json::{self, json};
use std::sync::atomic::{self, AtomicBool, AtomicUsize};
use std::{cell::RefCell, future::Future, rc::Rc, time::Instant};
use test::{build_editor_with_project, editor_lsp_test_context::rust_lang};
use unindent::Unindent;
use util::{
    assert_set_eq,
    test::{marked_text_ranges, marked_text_ranges_by, sample_text, TextRangeMarker},
};
use workspace::{
    item::{FollowEvent, FollowableItem, Item, ItemHandle},
    NavigationEntry, ViewId,
};

#[gpui::test]
fn test_edit_events(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let buffer = cx.new_model(|cx| {
        let mut buffer = language::Buffer::local("123456", cx);
        buffer.set_group_interval(Duration::from_secs(1));
        buffer
    });

    let events = Rc::new(RefCell::new(Vec::new()));
    let editor1 = cx.add_window({
        let events = events.clone();
        |cx| {
            let view = cx.view().clone();
            cx.subscribe(&view, move |_, _, event: &EditorEvent, _| match event {
                EditorEvent::Edited { .. } => events.borrow_mut().push(("editor1", "edited")),
                EditorEvent::BufferEdited => events.borrow_mut().push(("editor1", "buffer edited")),
                _ => {}
            })
            .detach();
            Editor::for_buffer(buffer.clone(), None, cx)
        }
    });

    let editor2 = cx.add_window({
        let events = events.clone();
        |cx| {
            cx.subscribe(
                &cx.view().clone(),
                move |_, _, event: &EditorEvent, _| match event {
                    EditorEvent::Edited { .. } => events.borrow_mut().push(("editor2", "edited")),
                    EditorEvent::BufferEdited => {
                        events.borrow_mut().push(("editor2", "buffer edited"))
                    }
                    _ => {}
                },
            )
            .detach();
            Editor::for_buffer(buffer.clone(), None, cx)
        }
    });

    assert_eq!(mem::take(&mut *events.borrow_mut()), []);

    // Mutating editor 1 will emit an `Edited` event only for that editor.
    _ = editor1.update(cx, |editor, cx| editor.insert("X", cx));
    assert_eq!(
        mem::take(&mut *events.borrow_mut()),
        [
            ("editor1", "edited"),
            ("editor1", "buffer edited"),
            ("editor2", "buffer edited"),
        ]
    );

    // Mutating editor 2 will emit an `Edited` event only for that editor.
    _ = editor2.update(cx, |editor, cx| editor.delete(&Delete, cx));
    assert_eq!(
        mem::take(&mut *events.borrow_mut()),
        [
            ("editor2", "edited"),
            ("editor1", "buffer edited"),
            ("editor2", "buffer edited"),
        ]
    );

    // Undoing on editor 1 will emit an `Edited` event only for that editor.
    _ = editor1.update(cx, |editor, cx| editor.undo(&Undo, cx));
    assert_eq!(
        mem::take(&mut *events.borrow_mut()),
        [
            ("editor1", "edited"),
            ("editor1", "buffer edited"),
            ("editor2", "buffer edited"),
        ]
    );

    // Redoing on editor 1 will emit an `Edited` event only for that editor.
    _ = editor1.update(cx, |editor, cx| editor.redo(&Redo, cx));
    assert_eq!(
        mem::take(&mut *events.borrow_mut()),
        [
            ("editor1", "edited"),
            ("editor1", "buffer edited"),
            ("editor2", "buffer edited"),
        ]
    );

    // Undoing on editor 2 will emit an `Edited` event only for that editor.
    _ = editor2.update(cx, |editor, cx| editor.undo(&Undo, cx));
    assert_eq!(
        mem::take(&mut *events.borrow_mut()),
        [
            ("editor2", "edited"),
            ("editor1", "buffer edited"),
            ("editor2", "buffer edited"),
        ]
    );

    // Redoing on editor 2 will emit an `Edited` event only for that editor.
    _ = editor2.update(cx, |editor, cx| editor.redo(&Redo, cx));
    assert_eq!(
        mem::take(&mut *events.borrow_mut()),
        [
            ("editor2", "edited"),
            ("editor1", "buffer edited"),
            ("editor2", "buffer edited"),
        ]
    );

    // No event is emitted when the mutation is a no-op.
    _ = editor2.update(cx, |editor, cx| {
        editor.change_selections(None, cx, |s| s.select_ranges([0..0]));

        editor.backspace(&Backspace, cx);
    });
    assert_eq!(mem::take(&mut *events.borrow_mut()), []);
}

#[gpui::test]
fn test_undo_redo_with_selection_restoration(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let mut now = Instant::now();
    let group_interval = Duration::from_millis(1);
    let buffer = cx.new_model(|cx| {
        let mut buf = language::Buffer::local("123456", cx);
        buf.set_group_interval(group_interval);
        buf
    });
    let buffer = cx.new_model(|cx| MultiBuffer::singleton(buffer, cx));
    let editor = cx.add_window(|cx| build_editor(buffer.clone(), cx));

    _ = editor.update(cx, |editor, cx| {
        editor.start_transaction_at(now, cx);
        editor.change_selections(None, cx, |s| s.select_ranges([2..4]));

        editor.insert("cd", cx);
        editor.end_transaction_at(now, cx);
        assert_eq!(editor.text(cx), "12cd56");
        assert_eq!(editor.selections.ranges(cx), vec![4..4]);

        editor.start_transaction_at(now, cx);
        editor.change_selections(None, cx, |s| s.select_ranges([4..5]));
        editor.insert("e", cx);
        editor.end_transaction_at(now, cx);
        assert_eq!(editor.text(cx), "12cde6");
        assert_eq!(editor.selections.ranges(cx), vec![5..5]);

        now += group_interval + Duration::from_millis(1);
        editor.change_selections(None, cx, |s| s.select_ranges([2..2]));

        // Simulate an edit in another editor
        buffer.update(cx, |buffer, cx| {
            buffer.start_transaction_at(now, cx);
            buffer.edit([(0..1, "a")], None, cx);
            buffer.edit([(1..1, "b")], None, cx);
            buffer.end_transaction_at(now, cx);
        });

        assert_eq!(editor.text(cx), "ab2cde6");
        assert_eq!(editor.selections.ranges(cx), vec![3..3]);

        // Last transaction happened past the group interval in a different editor.
        // Undo it individually and don't restore selections.
        editor.undo(&Undo, cx);
        assert_eq!(editor.text(cx), "12cde6");
        assert_eq!(editor.selections.ranges(cx), vec![2..2]);

        // First two transactions happened within the group interval in this editor.
        // Undo them together and restore selections.
        editor.undo(&Undo, cx);
        editor.undo(&Undo, cx); // Undo stack is empty here, so this is a no-op.
        assert_eq!(editor.text(cx), "123456");
        assert_eq!(editor.selections.ranges(cx), vec![0..0]);

        // Redo the first two transactions together.
        editor.redo(&Redo, cx);
        assert_eq!(editor.text(cx), "12cde6");
        assert_eq!(editor.selections.ranges(cx), vec![5..5]);

        // Redo the last transaction on its own.
        editor.redo(&Redo, cx);
        assert_eq!(editor.text(cx), "ab2cde6");
        assert_eq!(editor.selections.ranges(cx), vec![6..6]);

        // Test empty transactions.
        editor.start_transaction_at(now, cx);
        editor.end_transaction_at(now, cx);
        editor.undo(&Undo, cx);
        assert_eq!(editor.text(cx), "12cde6");
    });
}

#[gpui::test]
fn test_ime_composition(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let buffer = cx.new_model(|cx| {
        let mut buffer = language::Buffer::local("abcde", cx);
        // Ensure automatic grouping doesn't occur.
        buffer.set_group_interval(Duration::ZERO);
        buffer
    });

    let buffer = cx.new_model(|cx| MultiBuffer::singleton(buffer, cx));
    cx.add_window(|cx| {
        let mut editor = build_editor(buffer.clone(), cx);

        // Start a new IME composition.
        editor.replace_and_mark_text_in_range(Some(0..1), "à", None, cx);
        editor.replace_and_mark_text_in_range(Some(0..1), "á", None, cx);
        editor.replace_and_mark_text_in_range(Some(0..1), "ä", None, cx);
        assert_eq!(editor.text(cx), "äbcde");
        assert_eq!(
            editor.marked_text_ranges(cx),
            Some(vec![OffsetUtf16(0)..OffsetUtf16(1)])
        );

        // Finalize IME composition.
        editor.replace_text_in_range(None, "ā", cx);
        assert_eq!(editor.text(cx), "ābcde");
        assert_eq!(editor.marked_text_ranges(cx), None);

        // IME composition edits are grouped and are undone/redone at once.
        editor.undo(&Default::default(), cx);
        assert_eq!(editor.text(cx), "abcde");
        assert_eq!(editor.marked_text_ranges(cx), None);
        editor.redo(&Default::default(), cx);
        assert_eq!(editor.text(cx), "ābcde");
        assert_eq!(editor.marked_text_ranges(cx), None);

        // Start a new IME composition.
        editor.replace_and_mark_text_in_range(Some(0..1), "à", None, cx);
        assert_eq!(
            editor.marked_text_ranges(cx),
            Some(vec![OffsetUtf16(0)..OffsetUtf16(1)])
        );

        // Undoing during an IME composition cancels it.
        editor.undo(&Default::default(), cx);
        assert_eq!(editor.text(cx), "ābcde");
        assert_eq!(editor.marked_text_ranges(cx), None);

        // Start a new IME composition with an invalid marked range, ensuring it gets clipped.
        editor.replace_and_mark_text_in_range(Some(4..999), "è", None, cx);
        assert_eq!(editor.text(cx), "ābcdè");
        assert_eq!(
            editor.marked_text_ranges(cx),
            Some(vec![OffsetUtf16(4)..OffsetUtf16(5)])
        );

        // Finalize IME composition with an invalid replacement range, ensuring it gets clipped.
        editor.replace_text_in_range(Some(4..999), "ę", cx);
        assert_eq!(editor.text(cx), "ābcdę");
        assert_eq!(editor.marked_text_ranges(cx), None);

        // Start a new IME composition with multiple cursors.
        editor.change_selections(None, cx, |s| {
            s.select_ranges([
                OffsetUtf16(1)..OffsetUtf16(1),
                OffsetUtf16(3)..OffsetUtf16(3),
                OffsetUtf16(5)..OffsetUtf16(5),
            ])
        });
        editor.replace_and_mark_text_in_range(Some(4..5), "XYZ", None, cx);
        assert_eq!(editor.text(cx), "XYZbXYZdXYZ");
        assert_eq!(
            editor.marked_text_ranges(cx),
            Some(vec![
                OffsetUtf16(0)..OffsetUtf16(3),
                OffsetUtf16(4)..OffsetUtf16(7),
                OffsetUtf16(8)..OffsetUtf16(11)
            ])
        );

        // Ensure the newly-marked range gets treated as relative to the previously-marked ranges.
        editor.replace_and_mark_text_in_range(Some(1..2), "1", None, cx);
        assert_eq!(editor.text(cx), "X1ZbX1ZdX1Z");
        assert_eq!(
            editor.marked_text_ranges(cx),
            Some(vec![
                OffsetUtf16(1)..OffsetUtf16(2),
                OffsetUtf16(5)..OffsetUtf16(6),
                OffsetUtf16(9)..OffsetUtf16(10)
            ])
        );

        // Finalize IME composition with multiple cursors.
        editor.replace_text_in_range(Some(9..10), "2", cx);
        assert_eq!(editor.text(cx), "X2ZbX2ZdX2Z");
        assert_eq!(editor.marked_text_ranges(cx), None);

        editor
    });
}

#[gpui::test]
fn test_selection_with_mouse(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let editor = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple("aaaaaa\nbbbbbb\ncccccc\nddddddd\n", cx);
        build_editor(buffer, cx)
    });

    _ = editor.update(cx, |view, cx| {
        view.begin_selection(DisplayPoint::new(DisplayRow(2), 2), false, 1, cx);
    });
    assert_eq!(
        editor
            .update(cx, |view, cx| view.selections.display_ranges(cx))
            .unwrap(),
        [DisplayPoint::new(DisplayRow(2), 2)..DisplayPoint::new(DisplayRow(2), 2)]
    );

    _ = editor.update(cx, |view, cx| {
        view.update_selection(
            DisplayPoint::new(DisplayRow(3), 3),
            0,
            gpui::Point::<f32>::default(),
            cx,
        );
    });

    assert_eq!(
        editor
            .update(cx, |view, cx| view.selections.display_ranges(cx))
            .unwrap(),
        [DisplayPoint::new(DisplayRow(2), 2)..DisplayPoint::new(DisplayRow(3), 3)]
    );

    _ = editor.update(cx, |view, cx| {
        view.update_selection(
            DisplayPoint::new(DisplayRow(1), 1),
            0,
            gpui::Point::<f32>::default(),
            cx,
        );
    });

    assert_eq!(
        editor
            .update(cx, |view, cx| view.selections.display_ranges(cx))
            .unwrap(),
        [DisplayPoint::new(DisplayRow(2), 2)..DisplayPoint::new(DisplayRow(1), 1)]
    );

    _ = editor.update(cx, |view, cx| {
        view.end_selection(cx);
        view.update_selection(
            DisplayPoint::new(DisplayRow(3), 3),
            0,
            gpui::Point::<f32>::default(),
            cx,
        );
    });

    assert_eq!(
        editor
            .update(cx, |view, cx| view.selections.display_ranges(cx))
            .unwrap(),
        [DisplayPoint::new(DisplayRow(2), 2)..DisplayPoint::new(DisplayRow(1), 1)]
    );

    _ = editor.update(cx, |view, cx| {
        view.begin_selection(DisplayPoint::new(DisplayRow(3), 3), true, 1, cx);
        view.update_selection(
            DisplayPoint::new(DisplayRow(0), 0),
            0,
            gpui::Point::<f32>::default(),
            cx,
        );
    });

    assert_eq!(
        editor
            .update(cx, |view, cx| view.selections.display_ranges(cx))
            .unwrap(),
        [
            DisplayPoint::new(DisplayRow(2), 2)..DisplayPoint::new(DisplayRow(1), 1),
            DisplayPoint::new(DisplayRow(3), 3)..DisplayPoint::new(DisplayRow(0), 0)
        ]
    );

    _ = editor.update(cx, |view, cx| {
        view.end_selection(cx);
    });

    assert_eq!(
        editor
            .update(cx, |view, cx| view.selections.display_ranges(cx))
            .unwrap(),
        [DisplayPoint::new(DisplayRow(3), 3)..DisplayPoint::new(DisplayRow(0), 0)]
    );
}

#[gpui::test]
fn test_multiple_cursor_removal(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let editor = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple("aaaaaa\nbbbbbb\ncccccc\nddddddd\n", cx);
        build_editor(buffer, cx)
    });

    _ = editor.update(cx, |view, cx| {
        view.begin_selection(DisplayPoint::new(DisplayRow(2), 1), false, 1, cx);
    });

    _ = editor.update(cx, |view, cx| {
        view.end_selection(cx);
    });

    _ = editor.update(cx, |view, cx| {
        view.begin_selection(DisplayPoint::new(DisplayRow(3), 2), true, 1, cx);
    });

    _ = editor.update(cx, |view, cx| {
        view.end_selection(cx);
    });

    assert_eq!(
        editor
            .update(cx, |view, cx| view.selections.display_ranges(cx))
            .unwrap(),
        [
            DisplayPoint::new(DisplayRow(2), 1)..DisplayPoint::new(DisplayRow(2), 1),
            DisplayPoint::new(DisplayRow(3), 2)..DisplayPoint::new(DisplayRow(3), 2)
        ]
    );

    _ = editor.update(cx, |view, cx| {
        view.begin_selection(DisplayPoint::new(DisplayRow(2), 1), true, 1, cx);
    });

    _ = editor.update(cx, |view, cx| {
        view.end_selection(cx);
    });

    assert_eq!(
        editor
            .update(cx, |view, cx| view.selections.display_ranges(cx))
            .unwrap(),
        [DisplayPoint::new(DisplayRow(3), 2)..DisplayPoint::new(DisplayRow(3), 2)]
    );
}

#[gpui::test]
fn test_canceling_pending_selection(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let view = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple("aaaaaa\nbbbbbb\ncccccc\ndddddd\n", cx);
        build_editor(buffer, cx)
    });

    _ = view.update(cx, |view, cx| {
        view.begin_selection(DisplayPoint::new(DisplayRow(2), 2), false, 1, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            [DisplayPoint::new(DisplayRow(2), 2)..DisplayPoint::new(DisplayRow(2), 2)]
        );
    });

    _ = view.update(cx, |view, cx| {
        view.update_selection(
            DisplayPoint::new(DisplayRow(3), 3),
            0,
            gpui::Point::<f32>::default(),
            cx,
        );
        assert_eq!(
            view.selections.display_ranges(cx),
            [DisplayPoint::new(DisplayRow(2), 2)..DisplayPoint::new(DisplayRow(3), 3)]
        );
    });

    _ = view.update(cx, |view, cx| {
        view.cancel(&Cancel, cx);
        view.update_selection(
            DisplayPoint::new(DisplayRow(1), 1),
            0,
            gpui::Point::<f32>::default(),
            cx,
        );
        assert_eq!(
            view.selections.display_ranges(cx),
            [DisplayPoint::new(DisplayRow(2), 2)..DisplayPoint::new(DisplayRow(3), 3)]
        );
    });
}

#[gpui::test]
fn test_movement_actions_with_pending_selection(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let view = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple("aaaaaa\nbbbbbb\ncccccc\ndddddd\n", cx);
        build_editor(buffer, cx)
    });

    _ = view.update(cx, |view, cx| {
        view.begin_selection(DisplayPoint::new(DisplayRow(2), 2), false, 1, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            [DisplayPoint::new(DisplayRow(2), 2)..DisplayPoint::new(DisplayRow(2), 2)]
        );

        view.move_down(&Default::default(), cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            [DisplayPoint::new(DisplayRow(3), 2)..DisplayPoint::new(DisplayRow(3), 2)]
        );

        view.begin_selection(DisplayPoint::new(DisplayRow(2), 2), false, 1, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            [DisplayPoint::new(DisplayRow(2), 2)..DisplayPoint::new(DisplayRow(2), 2)]
        );

        view.move_up(&Default::default(), cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            [DisplayPoint::new(DisplayRow(1), 2)..DisplayPoint::new(DisplayRow(1), 2)]
        );
    });
}

#[gpui::test]
fn test_clone(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let (text, selection_ranges) = marked_text_ranges(
        indoc! {"
            one
            two
            threeˇ
            four
            fiveˇ
        "},
        true,
    );

    let editor = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple(&text, cx);
        build_editor(buffer, cx)
    });

    _ = editor.update(cx, |editor, cx| {
        editor.change_selections(None, cx, |s| s.select_ranges(selection_ranges.clone()));
        editor.fold_creases(
            vec![
                Crease::simple(Point::new(1, 0)..Point::new(2, 0), FoldPlaceholder::test()),
                Crease::simple(Point::new(3, 0)..Point::new(4, 0), FoldPlaceholder::test()),
            ],
            true,
            cx,
        );
    });

    let cloned_editor = editor
        .update(cx, |editor, cx| {
            cx.open_window(Default::default(), |cx| cx.new_view(|cx| editor.clone(cx)))
        })
        .unwrap()
        .unwrap();

    let snapshot = editor.update(cx, |e, cx| e.snapshot(cx)).unwrap();
    let cloned_snapshot = cloned_editor.update(cx, |e, cx| e.snapshot(cx)).unwrap();

    assert_eq!(
        cloned_editor
            .update(cx, |e, cx| e.display_text(cx))
            .unwrap(),
        editor.update(cx, |e, cx| e.display_text(cx)).unwrap()
    );
    assert_eq!(
        cloned_snapshot
            .folds_in_range(0..text.len())
            .collect::<Vec<_>>(),
        snapshot.folds_in_range(0..text.len()).collect::<Vec<_>>(),
    );
    assert_set_eq!(
        cloned_editor
            .update(cx, |editor, cx| editor.selections.ranges::<Point>(cx))
            .unwrap(),
        editor
            .update(cx, |editor, cx| editor.selections.ranges(cx))
            .unwrap()
    );
    assert_set_eq!(
        cloned_editor
            .update(cx, |e, cx| e.selections.display_ranges(cx))
            .unwrap(),
        editor
            .update(cx, |e, cx| e.selections.display_ranges(cx))
            .unwrap()
    );
}

#[gpui::test]
async fn test_navigation_history(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    use workspace::item::Item;

    let fs = FakeFs::new(cx.executor());
    let project = Project::test(fs, [], cx).await;
    let workspace = cx.add_window(|cx| Workspace::test_new(project, cx));
    let pane = workspace
        .update(cx, |workspace, _| workspace.active_pane().clone())
        .unwrap();

    _ = workspace.update(cx, |_v, cx| {
        cx.new_view(|cx| {
            let buffer = MultiBuffer::build_simple(&sample_text(300, 5, 'a'), cx);
            let mut editor = build_editor(buffer.clone(), cx);
            let handle = cx.view();
            editor.set_nav_history(Some(pane.read(cx).nav_history_for_item(handle)));

            fn pop_history(editor: &mut Editor, cx: &mut WindowContext) -> Option<NavigationEntry> {
                editor.nav_history.as_mut().unwrap().pop_backward(cx)
            }

            // Move the cursor a small distance.
            // Nothing is added to the navigation history.
            editor.change_selections(None, cx, |s| {
                s.select_display_ranges([
                    DisplayPoint::new(DisplayRow(1), 0)..DisplayPoint::new(DisplayRow(1), 0)
                ])
            });
            editor.change_selections(None, cx, |s| {
                s.select_display_ranges([
                    DisplayPoint::new(DisplayRow(3), 0)..DisplayPoint::new(DisplayRow(3), 0)
                ])
            });
            assert!(pop_history(&mut editor, cx).is_none());

            // Move the cursor a large distance.
            // The history can jump back to the previous position.
            editor.change_selections(None, cx, |s| {
                s.select_display_ranges([
                    DisplayPoint::new(DisplayRow(13), 0)..DisplayPoint::new(DisplayRow(13), 3)
                ])
            });
            let nav_entry = pop_history(&mut editor, cx).unwrap();
            editor.navigate(nav_entry.data.unwrap(), cx);
            assert_eq!(nav_entry.item.id(), cx.entity_id());
            assert_eq!(
                editor.selections.display_ranges(cx),
                &[DisplayPoint::new(DisplayRow(3), 0)..DisplayPoint::new(DisplayRow(3), 0)]
            );
            assert!(pop_history(&mut editor, cx).is_none());

            // Move the cursor a small distance via the mouse.
            // Nothing is added to the navigation history.
            editor.begin_selection(DisplayPoint::new(DisplayRow(5), 0), false, 1, cx);
            editor.end_selection(cx);
            assert_eq!(
                editor.selections.display_ranges(cx),
                &[DisplayPoint::new(DisplayRow(5), 0)..DisplayPoint::new(DisplayRow(5), 0)]
            );
            assert!(pop_history(&mut editor, cx).is_none());

            // Move the cursor a large distance via the mouse.
            // The history can jump back to the previous position.
            editor.begin_selection(DisplayPoint::new(DisplayRow(15), 0), false, 1, cx);
            editor.end_selection(cx);
            assert_eq!(
                editor.selections.display_ranges(cx),
                &[DisplayPoint::new(DisplayRow(15), 0)..DisplayPoint::new(DisplayRow(15), 0)]
            );
            let nav_entry = pop_history(&mut editor, cx).unwrap();
            editor.navigate(nav_entry.data.unwrap(), cx);
            assert_eq!(nav_entry.item.id(), cx.entity_id());
            assert_eq!(
                editor.selections.display_ranges(cx),
                &[DisplayPoint::new(DisplayRow(5), 0)..DisplayPoint::new(DisplayRow(5), 0)]
            );
            assert!(pop_history(&mut editor, cx).is_none());

            // Set scroll position to check later
            editor.set_scroll_position(gpui::Point::<f32>::new(5.5, 5.5), cx);
            let original_scroll_position = editor.scroll_manager.anchor();

            // Jump to the end of the document and adjust scroll
            editor.move_to_end(&MoveToEnd, cx);
            editor.set_scroll_position(gpui::Point::<f32>::new(-2.5, -0.5), cx);
            assert_ne!(editor.scroll_manager.anchor(), original_scroll_position);

            let nav_entry = pop_history(&mut editor, cx).unwrap();
            editor.navigate(nav_entry.data.unwrap(), cx);
            assert_eq!(editor.scroll_manager.anchor(), original_scroll_position);

            // Ensure we don't panic when navigation data contains invalid anchors *and* points.
            let mut invalid_anchor = editor.scroll_manager.anchor().anchor;
            invalid_anchor.text_anchor.buffer_id = BufferId::new(999).ok();
            let invalid_point = Point::new(9999, 0);
            editor.navigate(
                Box::new(NavigationData {
                    cursor_anchor: invalid_anchor,
                    cursor_position: invalid_point,
                    scroll_anchor: ScrollAnchor {
                        anchor: invalid_anchor,
                        offset: Default::default(),
                    },
                    scroll_top_row: invalid_point.row,
                }),
                cx,
            );
            assert_eq!(
                editor.selections.display_ranges(cx),
                &[editor.max_point(cx)..editor.max_point(cx)]
            );
            assert_eq!(
                editor.scroll_position(cx),
                gpui::Point::new(0., editor.max_point(cx).row().as_f32())
            );

            editor
        })
    });
}

#[gpui::test]
fn test_cancel(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let view = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple("aaaaaa\nbbbbbb\ncccccc\ndddddd\n", cx);
        build_editor(buffer, cx)
    });

    _ = view.update(cx, |view, cx| {
        view.begin_selection(DisplayPoint::new(DisplayRow(3), 4), false, 1, cx);
        view.update_selection(
            DisplayPoint::new(DisplayRow(1), 1),
            0,
            gpui::Point::<f32>::default(),
            cx,
        );
        view.end_selection(cx);

        view.begin_selection(DisplayPoint::new(DisplayRow(0), 1), true, 1, cx);
        view.update_selection(
            DisplayPoint::new(DisplayRow(0), 3),
            0,
            gpui::Point::<f32>::default(),
            cx,
        );
        view.end_selection(cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            [
                DisplayPoint::new(DisplayRow(0), 1)..DisplayPoint::new(DisplayRow(0), 3),
                DisplayPoint::new(DisplayRow(3), 4)..DisplayPoint::new(DisplayRow(1), 1),
            ]
        );
    });

    _ = view.update(cx, |view, cx| {
        view.cancel(&Cancel, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            [DisplayPoint::new(DisplayRow(3), 4)..DisplayPoint::new(DisplayRow(1), 1)]
        );
    });

    _ = view.update(cx, |view, cx| {
        view.cancel(&Cancel, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            [DisplayPoint::new(DisplayRow(1), 1)..DisplayPoint::new(DisplayRow(1), 1)]
        );
    });
}

#[gpui::test]
fn test_fold_action(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let view = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple(
            &"
                impl Foo {
                    // Hello!

                    fn a() {
                        1
                    }

                    fn b() {
                        2
                    }

                    fn c() {
                        3
                    }
                }
            "
            .unindent(),
            cx,
        );
        build_editor(buffer.clone(), cx)
    });

    _ = view.update(cx, |view, cx| {
        view.change_selections(None, cx, |s| {
            s.select_display_ranges([
                DisplayPoint::new(DisplayRow(7), 0)..DisplayPoint::new(DisplayRow(12), 0)
            ]);
        });
        view.fold(&Fold, cx);
        assert_eq!(
            view.display_text(cx),
            "
                impl Foo {
                    // Hello!

                    fn a() {
                        1
                    }

                    fn b() {⋯
                    }

                    fn c() {⋯
                    }
                }
            "
            .unindent(),
        );

        view.fold(&Fold, cx);
        assert_eq!(
            view.display_text(cx),
            "
                impl Foo {⋯
                }
            "
            .unindent(),
        );

        view.unfold_lines(&UnfoldLines, cx);
        assert_eq!(
            view.display_text(cx),
            "
                impl Foo {
                    // Hello!

                    fn a() {
                        1
                    }

                    fn b() {⋯
                    }

                    fn c() {⋯
                    }
                }
            "
            .unindent(),
        );

        view.unfold_lines(&UnfoldLines, cx);
        assert_eq!(view.display_text(cx), view.buffer.read(cx).read(cx).text());
    });
}

#[gpui::test]
fn test_fold_action_whitespace_sensitive_language(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let view = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple(
            &"
                class Foo:
                    # Hello!

                    def a():
                        print(1)

                    def b():
                        print(2)

                    def c():
                        print(3)
            "
            .unindent(),
            cx,
        );
        build_editor(buffer.clone(), cx)
    });

    _ = view.update(cx, |view, cx| {
        view.change_selections(None, cx, |s| {
            s.select_display_ranges([
                DisplayPoint::new(DisplayRow(6), 0)..DisplayPoint::new(DisplayRow(10), 0)
            ]);
        });
        view.fold(&Fold, cx);
        assert_eq!(
            view.display_text(cx),
            "
                class Foo:
                    # Hello!

                    def a():
                        print(1)

                    def b():⋯

                    def c():⋯
            "
            .unindent(),
        );

        view.fold(&Fold, cx);
        assert_eq!(
            view.display_text(cx),
            "
                class Foo:⋯
            "
            .unindent(),
        );

        view.unfold_lines(&UnfoldLines, cx);
        assert_eq!(
            view.display_text(cx),
            "
                class Foo:
                    # Hello!

                    def a():
                        print(1)

                    def b():⋯

                    def c():⋯
            "
            .unindent(),
        );

        view.unfold_lines(&UnfoldLines, cx);
        assert_eq!(view.display_text(cx), view.buffer.read(cx).read(cx).text());
    });
}

#[gpui::test]
fn test_fold_action_multiple_line_breaks(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let view = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple(
            &"
                class Foo:
                    # Hello!

                    def a():
                        print(1)

                    def b():
                        print(2)


                    def c():
                        print(3)


            "
            .unindent(),
            cx,
        );
        build_editor(buffer.clone(), cx)
    });

    _ = view.update(cx, |view, cx| {
        view.change_selections(None, cx, |s| {
            s.select_display_ranges([
                DisplayPoint::new(DisplayRow(6), 0)..DisplayPoint::new(DisplayRow(11), 0)
            ]);
        });
        view.fold(&Fold, cx);
        assert_eq!(
            view.display_text(cx),
            "
                class Foo:
                    # Hello!

                    def a():
                        print(1)

                    def b():⋯


                    def c():⋯


            "
            .unindent(),
        );

        view.fold(&Fold, cx);
        assert_eq!(
            view.display_text(cx),
            "
                class Foo:⋯


            "
            .unindent(),
        );

        view.unfold_lines(&UnfoldLines, cx);
        assert_eq!(
            view.display_text(cx),
            "
                class Foo:
                    # Hello!

                    def a():
                        print(1)

                    def b():⋯


                    def c():⋯


            "
            .unindent(),
        );

        view.unfold_lines(&UnfoldLines, cx);
        assert_eq!(view.display_text(cx), view.buffer.read(cx).read(cx).text());
    });
}

#[gpui::test]
fn test_fold_at_level(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let view = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple(
            &"
                class Foo:
                    # Hello!

                    def a():
                        print(1)

                    def b():
                        print(2)


                class Bar:
                    # World!

                    def a():
                        print(1)

                    def b():
                        print(2)


            "
            .unindent(),
            cx,
        );
        build_editor(buffer.clone(), cx)
    });

    _ = view.update(cx, |view, cx| {
        view.fold_at_level(&FoldAtLevel { level: 2 }, cx);
        assert_eq!(
            view.display_text(cx),
            "
                class Foo:
                    # Hello!

                    def a():⋯

                    def b():⋯


                class Bar:
                    # World!

                    def a():⋯

                    def b():⋯


            "
            .unindent(),
        );

        view.fold_at_level(&FoldAtLevel { level: 1 }, cx);
        assert_eq!(
            view.display_text(cx),
            "
                class Foo:⋯


                class Bar:⋯


            "
            .unindent(),
        );

        view.unfold_all(&UnfoldAll, cx);
        view.fold_at_level(&FoldAtLevel { level: 0 }, cx);
        assert_eq!(
            view.display_text(cx),
            "
                class Foo:
                    # Hello!

                    def a():
                        print(1)

                    def b():
                        print(2)


                class Bar:
                    # World!

                    def a():
                        print(1)

                    def b():
                        print(2)


            "
            .unindent(),
        );

        assert_eq!(view.display_text(cx), view.buffer.read(cx).read(cx).text());
    });
}

#[gpui::test]
fn test_move_cursor(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let buffer = cx.update(|cx| MultiBuffer::build_simple(&sample_text(6, 6, 'a'), cx));
    let view = cx.add_window(|cx| build_editor(buffer.clone(), cx));

    buffer.update(cx, |buffer, cx| {
        buffer.edit(
            vec![
                (Point::new(1, 0)..Point::new(1, 0), "\t"),
                (Point::new(1, 1)..Point::new(1, 1), "\t"),
            ],
            None,
            cx,
        );
    });
    _ = view.update(cx, |view, cx| {
        assert_eq!(
            view.selections.display_ranges(cx),
            &[DisplayPoint::new(DisplayRow(0), 0)..DisplayPoint::new(DisplayRow(0), 0)]
        );

        view.move_down(&MoveDown, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[DisplayPoint::new(DisplayRow(1), 0)..DisplayPoint::new(DisplayRow(1), 0)]
        );

        view.move_right(&MoveRight, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[DisplayPoint::new(DisplayRow(1), 4)..DisplayPoint::new(DisplayRow(1), 4)]
        );

        view.move_left(&MoveLeft, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[DisplayPoint::new(DisplayRow(1), 0)..DisplayPoint::new(DisplayRow(1), 0)]
        );

        view.move_up(&MoveUp, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[DisplayPoint::new(DisplayRow(0), 0)..DisplayPoint::new(DisplayRow(0), 0)]
        );

        view.move_to_end(&MoveToEnd, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[DisplayPoint::new(DisplayRow(5), 6)..DisplayPoint::new(DisplayRow(5), 6)]
        );

        view.move_to_beginning(&MoveToBeginning, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[DisplayPoint::new(DisplayRow(0), 0)..DisplayPoint::new(DisplayRow(0), 0)]
        );

        view.change_selections(None, cx, |s| {
            s.select_display_ranges([
                DisplayPoint::new(DisplayRow(0), 1)..DisplayPoint::new(DisplayRow(0), 2)
            ]);
        });
        view.select_to_beginning(&SelectToBeginning, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[DisplayPoint::new(DisplayRow(0), 1)..DisplayPoint::new(DisplayRow(0), 0)]
        );

        view.select_to_end(&SelectToEnd, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[DisplayPoint::new(DisplayRow(0), 1)..DisplayPoint::new(DisplayRow(5), 6)]
        );
    });
}

// TODO: Re-enable this test
#[cfg(target_os = "macos")]
#[gpui::test]
fn test_move_cursor_multibyte(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let view = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple("ⓐⓑⓒⓓⓔ\nabcde\nαβγδε", cx);
        build_editor(buffer.clone(), cx)
    });

    assert_eq!('ⓐ'.len_utf8(), 3);
    assert_eq!('α'.len_utf8(), 2);

    _ = view.update(cx, |view, cx| {
        view.fold_creases(
            vec![
                Crease::simple(Point::new(0, 6)..Point::new(0, 12), FoldPlaceholder::test()),
                Crease::simple(Point::new(1, 2)..Point::new(1, 4), FoldPlaceholder::test()),
                Crease::simple(Point::new(2, 4)..Point::new(2, 8), FoldPlaceholder::test()),
            ],
            true,
            cx,
        );
        assert_eq!(view.display_text(cx), "ⓐⓑ⋯ⓔ\nab⋯e\nαβ⋯ε");

        view.move_right(&MoveRight, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[empty_range(0, "ⓐ".len())]
        );
        view.move_right(&MoveRight, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[empty_range(0, "ⓐⓑ".len())]
        );
        view.move_right(&MoveRight, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[empty_range(0, "ⓐⓑ⋯".len())]
        );

        view.move_down(&MoveDown, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[empty_range(1, "ab⋯e".len())]
        );
        view.move_left(&MoveLeft, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[empty_range(1, "ab⋯".len())]
        );
        view.move_left(&MoveLeft, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[empty_range(1, "ab".len())]
        );
        view.move_left(&MoveLeft, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[empty_range(1, "a".len())]
        );

        view.move_down(&MoveDown, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[empty_range(2, "α".len())]
        );
        view.move_right(&MoveRight, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[empty_range(2, "αβ".len())]
        );
        view.move_right(&MoveRight, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[empty_range(2, "αβ⋯".len())]
        );
        view.move_right(&MoveRight, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[empty_range(2, "αβ⋯ε".len())]
        );

        view.move_up(&MoveUp, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[empty_range(1, "ab⋯e".len())]
        );
        view.move_down(&MoveDown, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[empty_range(2, "αβ⋯ε".len())]
        );
        view.move_up(&MoveUp, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[empty_range(1, "ab⋯e".len())]
        );

        view.move_up(&MoveUp, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[empty_range(0, "ⓐⓑ".len())]
        );
        view.move_left(&MoveLeft, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[empty_range(0, "ⓐ".len())]
        );
        view.move_left(&MoveLeft, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[empty_range(0, "".len())]
        );
    });
}

#[gpui::test]
fn test_move_cursor_different_line_lengths(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let view = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple("ⓐⓑⓒⓓⓔ\nabcd\nαβγ\nabcd\nⓐⓑⓒⓓⓔ\n", cx);
        build_editor(buffer.clone(), cx)
    });
    _ = view.update(cx, |view, cx| {
        view.change_selections(None, cx, |s| {
            s.select_display_ranges([empty_range(0, "ⓐⓑⓒⓓⓔ".len())]);
        });

        // moving above start of document should move selection to start of document,
        // but the next move down should still be at the original goal_x
        view.move_up(&MoveUp, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[empty_range(0, "".len())]
        );

        view.move_down(&MoveDown, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[empty_range(1, "abcd".len())]
        );

        view.move_down(&MoveDown, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[empty_range(2, "αβγ".len())]
        );

        view.move_down(&MoveDown, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[empty_range(3, "abcd".len())]
        );

        view.move_down(&MoveDown, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[empty_range(4, "ⓐⓑⓒⓓⓔ".len())]
        );

        // moving past end of document should not change goal_x
        view.move_down(&MoveDown, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[empty_range(5, "".len())]
        );

        view.move_down(&MoveDown, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[empty_range(5, "".len())]
        );

        view.move_up(&MoveUp, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[empty_range(4, "ⓐⓑⓒⓓⓔ".len())]
        );

        view.move_up(&MoveUp, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[empty_range(3, "abcd".len())]
        );

        view.move_up(&MoveUp, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[empty_range(2, "αβγ".len())]
        );
    });
}

#[gpui::test]
fn test_beginning_end_of_line(cx: &mut TestAppContext) {
    init_test(cx, |_| {});
    let move_to_beg = MoveToBeginningOfLine {
        stop_at_soft_wraps: true,
    };

    let move_to_end = MoveToEndOfLine {
        stop_at_soft_wraps: true,
    };

    let view = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple("abc\n  def", cx);
        build_editor(buffer, cx)
    });
    _ = view.update(cx, |view, cx| {
        view.change_selections(None, cx, |s| {
            s.select_display_ranges([
                DisplayPoint::new(DisplayRow(0), 1)..DisplayPoint::new(DisplayRow(0), 1),
                DisplayPoint::new(DisplayRow(1), 4)..DisplayPoint::new(DisplayRow(1), 4),
            ]);
        });
    });

    _ = view.update(cx, |view, cx| {
        view.move_to_beginning_of_line(&move_to_beg, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[
                DisplayPoint::new(DisplayRow(0), 0)..DisplayPoint::new(DisplayRow(0), 0),
                DisplayPoint::new(DisplayRow(1), 2)..DisplayPoint::new(DisplayRow(1), 2),
            ]
        );
    });

    _ = view.update(cx, |view, cx| {
        view.move_to_beginning_of_line(&move_to_beg, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[
                DisplayPoint::new(DisplayRow(0), 0)..DisplayPoint::new(DisplayRow(0), 0),
                DisplayPoint::new(DisplayRow(1), 0)..DisplayPoint::new(DisplayRow(1), 0),
            ]
        );
    });

    _ = view.update(cx, |view, cx| {
        view.move_to_beginning_of_line(&move_to_beg, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[
                DisplayPoint::new(DisplayRow(0), 0)..DisplayPoint::new(DisplayRow(0), 0),
                DisplayPoint::new(DisplayRow(1), 2)..DisplayPoint::new(DisplayRow(1), 2),
            ]
        );
    });

    _ = view.update(cx, |view, cx| {
        view.move_to_end_of_line(&move_to_end, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[
                DisplayPoint::new(DisplayRow(0), 3)..DisplayPoint::new(DisplayRow(0), 3),
                DisplayPoint::new(DisplayRow(1), 5)..DisplayPoint::new(DisplayRow(1), 5),
            ]
        );
    });

    // Moving to the end of line again is a no-op.
    _ = view.update(cx, |view, cx| {
        view.move_to_end_of_line(&move_to_end, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[
                DisplayPoint::new(DisplayRow(0), 3)..DisplayPoint::new(DisplayRow(0), 3),
                DisplayPoint::new(DisplayRow(1), 5)..DisplayPoint::new(DisplayRow(1), 5),
            ]
        );
    });

    _ = view.update(cx, |view, cx| {
        view.move_left(&MoveLeft, cx);
        view.select_to_beginning_of_line(
            &SelectToBeginningOfLine {
                stop_at_soft_wraps: true,
            },
            cx,
        );
        assert_eq!(
            view.selections.display_ranges(cx),
            &[
                DisplayPoint::new(DisplayRow(0), 2)..DisplayPoint::new(DisplayRow(0), 0),
                DisplayPoint::new(DisplayRow(1), 4)..DisplayPoint::new(DisplayRow(1), 2),
            ]
        );
    });

    _ = view.update(cx, |view, cx| {
        view.select_to_beginning_of_line(
            &SelectToBeginningOfLine {
                stop_at_soft_wraps: true,
            },
            cx,
        );
        assert_eq!(
            view.selections.display_ranges(cx),
            &[
                DisplayPoint::new(DisplayRow(0), 2)..DisplayPoint::new(DisplayRow(0), 0),
                DisplayPoint::new(DisplayRow(1), 4)..DisplayPoint::new(DisplayRow(1), 0),
            ]
        );
    });

    _ = view.update(cx, |view, cx| {
        view.select_to_beginning_of_line(
            &SelectToBeginningOfLine {
                stop_at_soft_wraps: true,
            },
            cx,
        );
        assert_eq!(
            view.selections.display_ranges(cx),
            &[
                DisplayPoint::new(DisplayRow(0), 2)..DisplayPoint::new(DisplayRow(0), 0),
                DisplayPoint::new(DisplayRow(1), 4)..DisplayPoint::new(DisplayRow(1), 2),
            ]
        );
    });

    _ = view.update(cx, |view, cx| {
        view.select_to_end_of_line(
            &SelectToEndOfLine {
                stop_at_soft_wraps: true,
            },
            cx,
        );
        assert_eq!(
            view.selections.display_ranges(cx),
            &[
                DisplayPoint::new(DisplayRow(0), 2)..DisplayPoint::new(DisplayRow(0), 3),
                DisplayPoint::new(DisplayRow(1), 4)..DisplayPoint::new(DisplayRow(1), 5),
            ]
        );
    });

    _ = view.update(cx, |view, cx| {
        view.delete_to_end_of_line(&DeleteToEndOfLine, cx);
        assert_eq!(view.display_text(cx), "ab\n  de");
        assert_eq!(
            view.selections.display_ranges(cx),
            &[
                DisplayPoint::new(DisplayRow(0), 2)..DisplayPoint::new(DisplayRow(0), 2),
                DisplayPoint::new(DisplayRow(1), 4)..DisplayPoint::new(DisplayRow(1), 4),
            ]
        );
    });

    _ = view.update(cx, |view, cx| {
        view.delete_to_beginning_of_line(&DeleteToBeginningOfLine, cx);
        assert_eq!(view.display_text(cx), "\n");
        assert_eq!(
            view.selections.display_ranges(cx),
            &[
                DisplayPoint::new(DisplayRow(0), 0)..DisplayPoint::new(DisplayRow(0), 0),
                DisplayPoint::new(DisplayRow(1), 0)..DisplayPoint::new(DisplayRow(1), 0),
            ]
        );
    });
}

#[gpui::test]
fn test_beginning_end_of_line_ignore_soft_wrap(cx: &mut TestAppContext) {
    init_test(cx, |_| {});
    let move_to_beg = MoveToBeginningOfLine {
        stop_at_soft_wraps: false,
    };

    let move_to_end = MoveToEndOfLine {
        stop_at_soft_wraps: false,
    };

    let view = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple("thequickbrownfox\njumpedoverthelazydogs", cx);
        build_editor(buffer, cx)
    });

    _ = view.update(cx, |view, cx| {
        view.set_wrap_width(Some(140.0.into()), cx);

        // We expect the following lines after wrapping
        // ```
        // thequickbrownfox
        // jumpedoverthelazydo
        // gs
        // ```
        // The final `gs` was soft-wrapped onto a new line.
        assert_eq!(
            "thequickbrownfox\njumpedoverthelaz\nydogs",
            view.display_text(cx),
        );

        // First, let's assert behavior on the first line, that was not soft-wrapped.
        // Start the cursor at the `k` on the first line
        view.change_selections(None, cx, |s| {
            s.select_display_ranges([
                DisplayPoint::new(DisplayRow(0), 7)..DisplayPoint::new(DisplayRow(0), 7)
            ]);
        });

        // Moving to the beginning of the line should put us at the beginning of the line.
        view.move_to_beginning_of_line(&move_to_beg, cx);
        assert_eq!(
            vec![DisplayPoint::new(DisplayRow(0), 0)..DisplayPoint::new(DisplayRow(0), 0),],
            view.selections.display_ranges(cx)
        );

        // Moving to the end of the line should put us at the end of the line.
        view.move_to_end_of_line(&move_to_end, cx);
        assert_eq!(
            vec![DisplayPoint::new(DisplayRow(0), 16)..DisplayPoint::new(DisplayRow(0), 16),],
            view.selections.display_ranges(cx)
        );

        // Now, let's assert behavior on the second line, that ended up being soft-wrapped.
        // Start the cursor at the last line (`y` that was wrapped to a new line)
        view.change_selections(None, cx, |s| {
            s.select_display_ranges([
                DisplayPoint::new(DisplayRow(2), 0)..DisplayPoint::new(DisplayRow(2), 0)
            ]);
        });

        // Moving to the beginning of the line should put us at the start of the second line of
        // display text, i.e., the `j`.
        view.move_to_beginning_of_line(&move_to_beg, cx);
        assert_eq!(
            vec![DisplayPoint::new(DisplayRow(1), 0)..DisplayPoint::new(DisplayRow(1), 0),],
            view.selections.display_ranges(cx)
        );

        // Moving to the beginning of the line again should be a no-op.
        view.move_to_beginning_of_line(&move_to_beg, cx);
        assert_eq!(
            vec![DisplayPoint::new(DisplayRow(1), 0)..DisplayPoint::new(DisplayRow(1), 0),],
            view.selections.display_ranges(cx)
        );

        // Moving to the end of the line should put us right after the `s` that was soft-wrapped to the
        // next display line.
        view.move_to_end_of_line(&move_to_end, cx);
        assert_eq!(
            vec![DisplayPoint::new(DisplayRow(2), 5)..DisplayPoint::new(DisplayRow(2), 5),],
            view.selections.display_ranges(cx)
        );

        // Moving to the end of the line again should be a no-op.
        view.move_to_end_of_line(&move_to_end, cx);
        assert_eq!(
            vec![DisplayPoint::new(DisplayRow(2), 5)..DisplayPoint::new(DisplayRow(2), 5),],
            view.selections.display_ranges(cx)
        );
    });
}

#[gpui::test]
fn test_prev_next_word_boundary(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let view = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple("use std::str::{foo, bar}\n\n  {baz.qux()}", cx);
        build_editor(buffer, cx)
    });
    _ = view.update(cx, |view, cx| {
        view.change_selections(None, cx, |s| {
            s.select_display_ranges([
                DisplayPoint::new(DisplayRow(0), 11)..DisplayPoint::new(DisplayRow(0), 11),
                DisplayPoint::new(DisplayRow(2), 4)..DisplayPoint::new(DisplayRow(2), 4),
            ])
        });

        view.move_to_previous_word_start(&MoveToPreviousWordStart, cx);
        assert_selection_ranges("use std::ˇstr::{foo, bar}\n\n  {ˇbaz.qux()}", view, cx);

        view.move_to_previous_word_start(&MoveToPreviousWordStart, cx);
        assert_selection_ranges("use stdˇ::str::{foo, bar}\n\n  ˇ{baz.qux()}", view, cx);

        view.move_to_previous_word_start(&MoveToPreviousWordStart, cx);
        assert_selection_ranges("use ˇstd::str::{foo, bar}\n\nˇ  {baz.qux()}", view, cx);

        view.move_to_previous_word_start(&MoveToPreviousWordStart, cx);
        assert_selection_ranges("ˇuse std::str::{foo, bar}\nˇ\n  {baz.qux()}", view, cx);

        view.move_to_previous_word_start(&MoveToPreviousWordStart, cx);
        assert_selection_ranges("ˇuse std::str::{foo, barˇ}\n\n  {baz.qux()}", view, cx);

        view.move_to_next_word_end(&MoveToNextWordEnd, cx);
        assert_selection_ranges("useˇ std::str::{foo, bar}ˇ\n\n  {baz.qux()}", view, cx);

        view.move_to_next_word_end(&MoveToNextWordEnd, cx);
        assert_selection_ranges("use stdˇ::str::{foo, bar}\nˇ\n  {baz.qux()}", view, cx);

        view.move_to_next_word_end(&MoveToNextWordEnd, cx);
        assert_selection_ranges("use std::ˇstr::{foo, bar}\n\n  {ˇbaz.qux()}", view, cx);

        view.move_right(&MoveRight, cx);
        view.select_to_previous_word_start(&SelectToPreviousWordStart, cx);
        assert_selection_ranges("use std::«ˇs»tr::{foo, bar}\n\n  {«ˇb»az.qux()}", view, cx);

        view.select_to_previous_word_start(&SelectToPreviousWordStart, cx);
        assert_selection_ranges("use std«ˇ::s»tr::{foo, bar}\n\n  «ˇ{b»az.qux()}", view, cx);

        view.select_to_next_word_end(&SelectToNextWordEnd, cx);
        assert_selection_ranges("use std::«ˇs»tr::{foo, bar}\n\n  {«ˇb»az.qux()}", view, cx);
    });
}

#[gpui::test]
fn test_prev_next_word_bounds_with_soft_wrap(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let view = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple("use one::{\n    two::three::four::five\n};", cx);
        build_editor(buffer, cx)
    });

    _ = view.update(cx, |view, cx| {
        view.set_wrap_width(Some(140.0.into()), cx);
        assert_eq!(
            view.display_text(cx),
            "use one::{\n    two::three::\n    four::five\n};"
        );

        view.change_selections(None, cx, |s| {
            s.select_display_ranges([
                DisplayPoint::new(DisplayRow(1), 7)..DisplayPoint::new(DisplayRow(1), 7)
            ]);
        });

        view.move_to_next_word_end(&MoveToNextWordEnd, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[DisplayPoint::new(DisplayRow(1), 9)..DisplayPoint::new(DisplayRow(1), 9)]
        );

        view.move_to_next_word_end(&MoveToNextWordEnd, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[DisplayPoint::new(DisplayRow(1), 14)..DisplayPoint::new(DisplayRow(1), 14)]
        );

        view.move_to_next_word_end(&MoveToNextWordEnd, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[DisplayPoint::new(DisplayRow(2), 4)..DisplayPoint::new(DisplayRow(2), 4)]
        );

        view.move_to_next_word_end(&MoveToNextWordEnd, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[DisplayPoint::new(DisplayRow(2), 8)..DisplayPoint::new(DisplayRow(2), 8)]
        );

        view.move_to_previous_word_start(&MoveToPreviousWordStart, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[DisplayPoint::new(DisplayRow(2), 4)..DisplayPoint::new(DisplayRow(2), 4)]
        );

        view.move_to_previous_word_start(&MoveToPreviousWordStart, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[DisplayPoint::new(DisplayRow(1), 14)..DisplayPoint::new(DisplayRow(1), 14)]
        );
    });
}

#[gpui::test]
async fn test_move_start_of_paragraph_end_of_paragraph(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});
    let mut cx = EditorTestContext::new(cx).await;

    let line_height = cx.editor(|editor, cx| {
        editor
            .style()
            .unwrap()
            .text
            .line_height_in_pixels(cx.rem_size())
    });
    cx.simulate_window_resize(cx.window, size(px(100.), 4. * line_height));

    cx.set_state(
        &r#"ˇone
        two

        three
        fourˇ
        five

        six"#
            .unindent(),
    );

    cx.update_editor(|editor, cx| editor.move_to_end_of_paragraph(&MoveToEndOfParagraph, cx));
    cx.assert_editor_state(
        &r#"one
        two
        ˇ
        three
        four
        five
        ˇ
        six"#
            .unindent(),
    );

    cx.update_editor(|editor, cx| editor.move_to_end_of_paragraph(&MoveToEndOfParagraph, cx));
    cx.assert_editor_state(
        &r#"one
        two

        three
        four
        five
        ˇ
        sixˇ"#
            .unindent(),
    );

    cx.update_editor(|editor, cx| editor.move_to_end_of_paragraph(&MoveToEndOfParagraph, cx));
    cx.assert_editor_state(
        &r#"one
        two

        three
        four
        five

        sixˇ"#
            .unindent(),
    );

    cx.update_editor(|editor, cx| editor.move_to_start_of_paragraph(&MoveToStartOfParagraph, cx));
    cx.assert_editor_state(
        &r#"one
        two

        three
        four
        five
        ˇ
        six"#
            .unindent(),
    );

    cx.update_editor(|editor, cx| editor.move_to_start_of_paragraph(&MoveToStartOfParagraph, cx));
    cx.assert_editor_state(
        &r#"one
        two
        ˇ
        three
        four
        five

        six"#
            .unindent(),
    );

    cx.update_editor(|editor, cx| editor.move_to_start_of_paragraph(&MoveToStartOfParagraph, cx));
    cx.assert_editor_state(
        &r#"ˇone
        two

        three
        four
        five

        six"#
            .unindent(),
    );
}

#[gpui::test]
async fn test_scroll_page_up_page_down(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});
    let mut cx = EditorTestContext::new(cx).await;
    let line_height = cx.editor(|editor, cx| {
        editor
            .style()
            .unwrap()
            .text
            .line_height_in_pixels(cx.rem_size())
    });
    let window = cx.window;
    cx.simulate_window_resize(window, size(px(1000.), 4. * line_height + px(0.5)));

    cx.set_state(
        r#"ˇone
        two
        three
        four
        five
        six
        seven
        eight
        nine
        ten
        "#,
    );

    cx.update_editor(|editor, cx| {
        assert_eq!(
            editor.snapshot(cx).scroll_position(),
            gpui::Point::new(0., 0.)
        );
        editor.scroll_screen(&ScrollAmount::Page(1.), cx);
        assert_eq!(
            editor.snapshot(cx).scroll_position(),
            gpui::Point::new(0., 3.)
        );
        editor.scroll_screen(&ScrollAmount::Page(1.), cx);
        assert_eq!(
            editor.snapshot(cx).scroll_position(),
            gpui::Point::new(0., 6.)
        );
        editor.scroll_screen(&ScrollAmount::Page(-1.), cx);
        assert_eq!(
            editor.snapshot(cx).scroll_position(),
            gpui::Point::new(0., 3.)
        );

        editor.scroll_screen(&ScrollAmount::Page(-0.5), cx);
        assert_eq!(
            editor.snapshot(cx).scroll_position(),
            gpui::Point::new(0., 1.)
        );
        editor.scroll_screen(&ScrollAmount::Page(0.5), cx);
        assert_eq!(
            editor.snapshot(cx).scroll_position(),
            gpui::Point::new(0., 3.)
        );
    });
}

#[gpui::test]
async fn test_autoscroll(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});
    let mut cx = EditorTestContext::new(cx).await;

    let line_height = cx.update_editor(|editor, cx| {
        editor.set_vertical_scroll_margin(2, cx);
        editor
            .style()
            .unwrap()
            .text
            .line_height_in_pixels(cx.rem_size())
    });
    let window = cx.window;
    cx.simulate_window_resize(window, size(px(1000.), 6. * line_height));

    cx.set_state(
        r#"ˇone
            two
            three
            four
            five
            six
            seven
            eight
            nine
            ten
        "#,
    );
    cx.update_editor(|editor, cx| {
        assert_eq!(
            editor.snapshot(cx).scroll_position(),
            gpui::Point::new(0., 0.0)
        );
    });

    // Add a cursor below the visible area. Since both cursors cannot fit
    // on screen, the editor autoscrolls to reveal the newest cursor, and
    // allows the vertical scroll margin below that cursor.
    cx.update_editor(|editor, cx| {
        editor.change_selections(Some(Autoscroll::fit()), cx, |selections| {
            selections.select_ranges([
                Point::new(0, 0)..Point::new(0, 0),
                Point::new(6, 0)..Point::new(6, 0),
            ]);
        })
    });
    cx.update_editor(|editor, cx| {
        assert_eq!(
            editor.snapshot(cx).scroll_position(),
            gpui::Point::new(0., 3.0)
        );
    });

    // Move down. The editor cursor scrolls down to track the newest cursor.
    cx.update_editor(|editor, cx| {
        editor.move_down(&Default::default(), cx);
    });
    cx.update_editor(|editor, cx| {
        assert_eq!(
            editor.snapshot(cx).scroll_position(),
            gpui::Point::new(0., 4.0)
        );
    });

    // Add a cursor above the visible area. Since both cursors fit on screen,
    // the editor scrolls to show both.
    cx.update_editor(|editor, cx| {
        editor.change_selections(Some(Autoscroll::fit()), cx, |selections| {
            selections.select_ranges([
                Point::new(1, 0)..Point::new(1, 0),
                Point::new(6, 0)..Point::new(6, 0),
            ]);
        })
    });
    cx.update_editor(|editor, cx| {
        assert_eq!(
            editor.snapshot(cx).scroll_position(),
            gpui::Point::new(0., 1.0)
        );
    });
}

#[gpui::test]
async fn test_move_page_up_page_down(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});
    let mut cx = EditorTestContext::new(cx).await;

    let line_height = cx.editor(|editor, cx| {
        editor
            .style()
            .unwrap()
            .text
            .line_height_in_pixels(cx.rem_size())
    });
    let window = cx.window;
    cx.simulate_window_resize(window, size(px(100.), 4. * line_height));
    cx.set_state(
        &r#"
        ˇone
        two
        threeˇ
        four
        five
        six
        seven
        eight
        nine
        ten
        "#
        .unindent(),
    );

    cx.update_editor(|editor, cx| editor.move_page_down(&MovePageDown::default(), cx));
    cx.assert_editor_state(
        &r#"
        one
        two
        three
        ˇfour
        five
        sixˇ
        seven
        eight
        nine
        ten
        "#
        .unindent(),
    );

    cx.update_editor(|editor, cx| editor.move_page_down(&MovePageDown::default(), cx));
    cx.assert_editor_state(
        &r#"
        one
        two
        three
        four
        five
        six
        ˇseven
        eight
        nineˇ
        ten
        "#
        .unindent(),
    );

    cx.update_editor(|editor, cx| editor.move_page_up(&MovePageUp::default(), cx));
    cx.assert_editor_state(
        &r#"
        one
        two
        three
        ˇfour
        five
        sixˇ
        seven
        eight
        nine
        ten
        "#
        .unindent(),
    );

    cx.update_editor(|editor, cx| editor.move_page_up(&MovePageUp::default(), cx));
    cx.assert_editor_state(
        &r#"
        ˇone
        two
        threeˇ
        four
        five
        six
        seven
        eight
        nine
        ten
        "#
        .unindent(),
    );

    // Test select collapsing
    cx.update_editor(|editor, cx| {
        editor.move_page_down(&MovePageDown::default(), cx);
        editor.move_page_down(&MovePageDown::default(), cx);
        editor.move_page_down(&MovePageDown::default(), cx);
    });
    cx.assert_editor_state(
        &r#"
        one
        two
        three
        four
        five
        six
        seven
        eight
        nine
        ˇten
        ˇ"#
        .unindent(),
    );
}

#[gpui::test]
async fn test_delete_to_beginning_of_line(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});
    let mut cx = EditorTestContext::new(cx).await;
    cx.set_state("one «two threeˇ» four");
    cx.update_editor(|editor, cx| {
        editor.delete_to_beginning_of_line(&DeleteToBeginningOfLine, cx);
        assert_eq!(editor.text(cx), " four");
    });
}

#[gpui::test]
fn test_delete_to_word_boundary(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let view = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple("one two three four", cx);
        build_editor(buffer.clone(), cx)
    });

    _ = view.update(cx, |view, cx| {
        view.change_selections(None, cx, |s| {
            s.select_display_ranges([
                // an empty selection - the preceding word fragment is deleted
                DisplayPoint::new(DisplayRow(0), 2)..DisplayPoint::new(DisplayRow(0), 2),
                // characters selected - they are deleted
                DisplayPoint::new(DisplayRow(0), 9)..DisplayPoint::new(DisplayRow(0), 12),
            ])
        });
        view.delete_to_previous_word_start(
            &DeleteToPreviousWordStart {
                ignore_newlines: false,
            },
            cx,
        );
        assert_eq!(view.buffer.read(cx).read(cx).text(), "e two te four");
    });

    _ = view.update(cx, |view, cx| {
        view.change_selections(None, cx, |s| {
            s.select_display_ranges([
                // an empty selection - the following word fragment is deleted
                DisplayPoint::new(DisplayRow(0), 3)..DisplayPoint::new(DisplayRow(0), 3),
                // characters selected - they are deleted
                DisplayPoint::new(DisplayRow(0), 9)..DisplayPoint::new(DisplayRow(0), 10),
            ])
        });
        view.delete_to_next_word_end(
            &DeleteToNextWordEnd {
                ignore_newlines: false,
            },
            cx,
        );
        assert_eq!(view.buffer.read(cx).read(cx).text(), "e t te our");
    });
}

#[gpui::test]
fn test_delete_to_previous_word_start_or_newline(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let view = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple("one\n2\nthree\n4", cx);
        build_editor(buffer.clone(), cx)
    });
    let del_to_prev_word_start = DeleteToPreviousWordStart {
        ignore_newlines: false,
    };
    let del_to_prev_word_start_ignore_newlines = DeleteToPreviousWordStart {
        ignore_newlines: true,
    };

    _ = view.update(cx, |view, cx| {
        view.change_selections(None, cx, |s| {
            s.select_display_ranges([
                DisplayPoint::new(DisplayRow(3), 1)..DisplayPoint::new(DisplayRow(3), 1)
            ])
        });
        view.delete_to_previous_word_start(&del_to_prev_word_start, cx);
        assert_eq!(view.buffer.read(cx).read(cx).text(), "one\n2\nthree\n");
        view.delete_to_previous_word_start(&del_to_prev_word_start, cx);
        assert_eq!(view.buffer.read(cx).read(cx).text(), "one\n2\nthree");
        view.delete_to_previous_word_start(&del_to_prev_word_start, cx);
        assert_eq!(view.buffer.read(cx).read(cx).text(), "one\n2\n");
        view.delete_to_previous_word_start(&del_to_prev_word_start, cx);
        assert_eq!(view.buffer.read(cx).read(cx).text(), "one\n2");
        view.delete_to_previous_word_start(&del_to_prev_word_start_ignore_newlines, cx);
        assert_eq!(view.buffer.read(cx).read(cx).text(), "one\n");
        view.delete_to_previous_word_start(&del_to_prev_word_start_ignore_newlines, cx);
        assert_eq!(view.buffer.read(cx).read(cx).text(), "");
    });
}

#[gpui::test]
fn test_delete_to_next_word_end_or_newline(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let view = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple("\none\n   two\nthree\n   four", cx);
        build_editor(buffer.clone(), cx)
    });
    let del_to_next_word_end = DeleteToNextWordEnd {
        ignore_newlines: false,
    };
    let del_to_next_word_end_ignore_newlines = DeleteToNextWordEnd {
        ignore_newlines: true,
    };

    _ = view.update(cx, |view, cx| {
        view.change_selections(None, cx, |s| {
            s.select_display_ranges([
                DisplayPoint::new(DisplayRow(0), 0)..DisplayPoint::new(DisplayRow(0), 0)
            ])
        });
        view.delete_to_next_word_end(&del_to_next_word_end, cx);
        assert_eq!(
            view.buffer.read(cx).read(cx).text(),
            "one\n   two\nthree\n   four"
        );
        view.delete_to_next_word_end(&del_to_next_word_end, cx);
        assert_eq!(
            view.buffer.read(cx).read(cx).text(),
            "\n   two\nthree\n   four"
        );
        view.delete_to_next_word_end(&del_to_next_word_end, cx);
        assert_eq!(view.buffer.read(cx).read(cx).text(), "two\nthree\n   four");
        view.delete_to_next_word_end(&del_to_next_word_end, cx);
        assert_eq!(view.buffer.read(cx).read(cx).text(), "\nthree\n   four");
        view.delete_to_next_word_end(&del_to_next_word_end_ignore_newlines, cx);
        assert_eq!(view.buffer.read(cx).read(cx).text(), "\n   four");
        view.delete_to_next_word_end(&del_to_next_word_end_ignore_newlines, cx);
        assert_eq!(view.buffer.read(cx).read(cx).text(), "");
    });
}

#[gpui::test]
fn test_newline(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let view = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple("aaaa\n    bbbb\n", cx);
        build_editor(buffer.clone(), cx)
    });

    _ = view.update(cx, |view, cx| {
        view.change_selections(None, cx, |s| {
            s.select_display_ranges([
                DisplayPoint::new(DisplayRow(0), 2)..DisplayPoint::new(DisplayRow(0), 2),
                DisplayPoint::new(DisplayRow(1), 2)..DisplayPoint::new(DisplayRow(1), 2),
                DisplayPoint::new(DisplayRow(1), 6)..DisplayPoint::new(DisplayRow(1), 6),
            ])
        });

        view.newline(&Newline, cx);
        assert_eq!(view.text(cx), "aa\naa\n  \n    bb\n    bb\n");
    });
}

#[gpui::test]
fn test_newline_with_old_selections(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let editor = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple(
            "
                a
                b(
                    X
                )
                c(
                    X
                )
            "
            .unindent()
            .as_str(),
            cx,
        );
        let mut editor = build_editor(buffer.clone(), cx);
        editor.change_selections(None, cx, |s| {
            s.select_ranges([
                Point::new(2, 4)..Point::new(2, 5),
                Point::new(5, 4)..Point::new(5, 5),
            ])
        });
        editor
    });

    _ = editor.update(cx, |editor, cx| {
        // Edit the buffer directly, deleting ranges surrounding the editor's selections
        editor.buffer.update(cx, |buffer, cx| {
            buffer.edit(
                [
                    (Point::new(1, 2)..Point::new(3, 0), ""),
                    (Point::new(4, 2)..Point::new(6, 0), ""),
                ],
                None,
                cx,
            );
            assert_eq!(
                buffer.read(cx).text(),
                "
                    a
                    b()
                    c()
                "
                .unindent()
            );
        });
        assert_eq!(
            editor.selections.ranges(cx),
            &[
                Point::new(1, 2)..Point::new(1, 2),
                Point::new(2, 2)..Point::new(2, 2),
            ],
        );

        editor.newline(&Newline, cx);
        assert_eq!(
            editor.text(cx),
            "
                a
                b(
                )
                c(
                )
            "
            .unindent()
        );

        // The selections are moved after the inserted newlines
        assert_eq!(
            editor.selections.ranges(cx),
            &[
                Point::new(2, 0)..Point::new(2, 0),
                Point::new(4, 0)..Point::new(4, 0),
            ],
        );
    });
}

#[gpui::test]
async fn test_newline_above(cx: &mut gpui::TestAppContext) {
    init_test(cx, |settings| {
        settings.defaults.tab_size = NonZeroU32::new(4)
    });

    let language = Arc::new(
        Language::new(
            LanguageConfig::default(),
            Some(tree_sitter_rust::LANGUAGE.into()),
        )
        .with_indents_query(r#"(_ "(" ")" @end) @indent"#)
        .unwrap(),
    );

    let mut cx = EditorTestContext::new(cx).await;
    cx.update_buffer(|buffer, cx| buffer.set_language(Some(language), cx));
    cx.set_state(indoc! {"
        const a: ˇA = (
            (ˇ
                «const_functionˇ»(ˇ),
                so«mˇ»et«hˇ»ing_ˇelse,ˇ
            )ˇ
        ˇ);ˇ
    "});

    cx.update_editor(|e, cx| e.newline_above(&NewlineAbove, cx));
    cx.assert_editor_state(indoc! {"
        ˇ
        const a: A = (
            ˇ
            (
                ˇ
                ˇ
                const_function(),
                ˇ
                ˇ
                ˇ
                ˇ
                something_else,
                ˇ
            )
            ˇ
            ˇ
        );
    "});
}

#[gpui::test]
async fn test_newline_below(cx: &mut gpui::TestAppContext) {
    init_test(cx, |settings| {
        settings.defaults.tab_size = NonZeroU32::new(4)
    });

    let language = Arc::new(
        Language::new(
            LanguageConfig::default(),
            Some(tree_sitter_rust::LANGUAGE.into()),
        )
        .with_indents_query(r#"(_ "(" ")" @end) @indent"#)
        .unwrap(),
    );

    let mut cx = EditorTestContext::new(cx).await;
    cx.update_buffer(|buffer, cx| buffer.set_language(Some(language), cx));
    cx.set_state(indoc! {"
        const a: ˇA = (
            (ˇ
                «const_functionˇ»(ˇ),
                so«mˇ»et«hˇ»ing_ˇelse,ˇ
            )ˇ
        ˇ);ˇ
    "});

    cx.update_editor(|e, cx| e.newline_below(&NewlineBelow, cx));
    cx.assert_editor_state(indoc! {"
        const a: A = (
            ˇ
            (
                ˇ
                const_function(),
                ˇ
                ˇ
                something_else,
                ˇ
                ˇ
                ˇ
                ˇ
            )
            ˇ
        );
        ˇ
        ˇ
    "});
}

#[gpui::test]
async fn test_newline_comments(cx: &mut gpui::TestAppContext) {
    init_test(cx, |settings| {
        settings.defaults.tab_size = NonZeroU32::new(4)
    });

    let language = Arc::new(Language::new(
        LanguageConfig {
            line_comments: vec!["//".into()],
            ..LanguageConfig::default()
        },
        None,
    ));
    {
        let mut cx = EditorTestContext::new(cx).await;
        cx.update_buffer(|buffer, cx| buffer.set_language(Some(language), cx));
        cx.set_state(indoc! {"
        // Fooˇ
    "});

        cx.update_editor(|e, cx| e.newline(&Newline, cx));
        cx.assert_editor_state(indoc! {"
        // Foo
        //ˇ
    "});
        // Ensure that if cursor is before the comment start, we do not actually insert a comment prefix.
        cx.set_state(indoc! {"
        ˇ// Foo
    "});
        cx.update_editor(|e, cx| e.newline(&Newline, cx));
        cx.assert_editor_state(indoc! {"

        ˇ// Foo
    "});
    }
    // Ensure that comment continuations can be disabled.
    update_test_language_settings(cx, |settings| {
        settings.defaults.extend_comment_on_newline = Some(false);
    });
    let mut cx = EditorTestContext::new(cx).await;
    cx.set_state(indoc! {"
        // Fooˇ
    "});
    cx.update_editor(|e, cx| e.newline(&Newline, cx));
    cx.assert_editor_state(indoc! {"
        // Foo
        ˇ
    "});
}

#[gpui::test]
fn test_insert_with_old_selections(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let editor = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple("a( X ), b( Y ), c( Z )", cx);
        let mut editor = build_editor(buffer.clone(), cx);
        editor.change_selections(None, cx, |s| s.select_ranges([3..4, 11..12, 19..20]));
        editor
    });

    _ = editor.update(cx, |editor, cx| {
        // Edit the buffer directly, deleting ranges surrounding the editor's selections
        editor.buffer.update(cx, |buffer, cx| {
            buffer.edit([(2..5, ""), (10..13, ""), (18..21, "")], None, cx);
            assert_eq!(buffer.read(cx).text(), "a(), b(), c()".unindent());
        });
        assert_eq!(editor.selections.ranges(cx), &[2..2, 7..7, 12..12],);

        editor.insert("Z", cx);
        assert_eq!(editor.text(cx), "a(Z), b(Z), c(Z)");

        // The selections are moved after the inserted characters
        assert_eq!(editor.selections.ranges(cx), &[3..3, 9..9, 15..15],);
    });
}

#[gpui::test]
async fn test_tab(cx: &mut gpui::TestAppContext) {
    init_test(cx, |settings| {
        settings.defaults.tab_size = NonZeroU32::new(3)
    });

    let mut cx = EditorTestContext::new(cx).await;
    cx.set_state(indoc! {"
        ˇabˇc
        ˇ🏀ˇ🏀ˇefg
        dˇ
    "});
    cx.update_editor(|e, cx| e.tab(&Tab, cx));
    cx.assert_editor_state(indoc! {"
           ˇab ˇc
           ˇ🏀  ˇ🏀  ˇefg
        d  ˇ
    "});

    cx.set_state(indoc! {"
        a
        «🏀ˇ»🏀«🏀ˇ»🏀«🏀ˇ»
    "});
    cx.update_editor(|e, cx| e.tab(&Tab, cx));
    cx.assert_editor_state(indoc! {"
        a
           «🏀ˇ»🏀«🏀ˇ»🏀«🏀ˇ»
    "});
}

#[gpui::test]
async fn test_tab_in_leading_whitespace_auto_indents_lines(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;
    let language = Arc::new(
        Language::new(
            LanguageConfig::default(),
            Some(tree_sitter_rust::LANGUAGE.into()),
        )
        .with_indents_query(r#"(_ "(" ")" @end) @indent"#)
        .unwrap(),
    );
    cx.update_buffer(|buffer, cx| buffer.set_language(Some(language), cx));

    // cursors that are already at the suggested indent level insert
    // a soft tab. cursors that are to the left of the suggested indent
    // auto-indent their line.
    cx.set_state(indoc! {"
        ˇ
        const a: B = (
            c(
                d(
        ˇ
                )
        ˇ
        ˇ    )
        );
    "});
    cx.update_editor(|e, cx| e.tab(&Tab, cx));
    cx.assert_editor_state(indoc! {"
            ˇ
        const a: B = (
            c(
                d(
                    ˇ
                )
                ˇ
            ˇ)
        );
    "});

    // handle auto-indent when there are multiple cursors on the same line
    cx.set_state(indoc! {"
        const a: B = (
            c(
        ˇ    ˇ
        ˇ    )
        );
    "});
    cx.update_editor(|e, cx| e.tab(&Tab, cx));
    cx.assert_editor_state(indoc! {"
        const a: B = (
            c(
                ˇ
            ˇ)
        );
    "});
}

#[gpui::test]
async fn test_tab_with_mixed_whitespace(cx: &mut gpui::TestAppContext) {
    init_test(cx, |settings| {
        settings.defaults.tab_size = NonZeroU32::new(4)
    });

    let language = Arc::new(
        Language::new(
            LanguageConfig::default(),
            Some(tree_sitter_rust::LANGUAGE.into()),
        )
        .with_indents_query(r#"(_ "{" "}" @end) @indent"#)
        .unwrap(),
    );

    let mut cx = EditorTestContext::new(cx).await;
    cx.update_buffer(|buffer, cx| buffer.set_language(Some(language), cx));
    cx.set_state(indoc! {"
        fn a() {
            if b {
        \t ˇc
            }
        }
    "});

    cx.update_editor(|e, cx| e.tab(&Tab, cx));
    cx.assert_editor_state(indoc! {"
        fn a() {
            if b {
                ˇc
            }
        }
    "});
}

#[gpui::test]
async fn test_indent_outdent(cx: &mut gpui::TestAppContext) {
    init_test(cx, |settings| {
        settings.defaults.tab_size = NonZeroU32::new(4);
    });

    let mut cx = EditorTestContext::new(cx).await;

    cx.set_state(indoc! {"
          «oneˇ» «twoˇ»
        three
         four
    "});
    cx.update_editor(|e, cx| e.tab(&Tab, cx));
    cx.assert_editor_state(indoc! {"
            «oneˇ» «twoˇ»
        three
         four
    "});

    cx.update_editor(|e, cx| e.tab_prev(&TabPrev, cx));
    cx.assert_editor_state(indoc! {"
        «oneˇ» «twoˇ»
        three
         four
    "});

    // select across line ending
    cx.set_state(indoc! {"
        one two
        t«hree
        ˇ» four
    "});
    cx.update_editor(|e, cx| e.tab(&Tab, cx));
    cx.assert_editor_state(indoc! {"
        one two
            t«hree
        ˇ» four
    "});

    cx.update_editor(|e, cx| e.tab_prev(&TabPrev, cx));
    cx.assert_editor_state(indoc! {"
        one two
        t«hree
        ˇ» four
    "});

    // Ensure that indenting/outdenting works when the cursor is at column 0.
    cx.set_state(indoc! {"
        one two
        ˇthree
            four
    "});
    cx.update_editor(|e, cx| e.tab(&Tab, cx));
    cx.assert_editor_state(indoc! {"
        one two
            ˇthree
            four
    "});

    cx.set_state(indoc! {"
        one two
        ˇ    three
            four
    "});
    cx.update_editor(|e, cx| e.tab_prev(&TabPrev, cx));
    cx.assert_editor_state(indoc! {"
        one two
        ˇthree
            four
    "});
}

#[gpui::test]
async fn test_indent_outdent_with_hard_tabs(cx: &mut gpui::TestAppContext) {
    init_test(cx, |settings| {
        settings.defaults.hard_tabs = Some(true);
    });

    let mut cx = EditorTestContext::new(cx).await;

    // select two ranges on one line
    cx.set_state(indoc! {"
        «oneˇ» «twoˇ»
        three
        four
    "});
    cx.update_editor(|e, cx| e.tab(&Tab, cx));
    cx.assert_editor_state(indoc! {"
        \t«oneˇ» «twoˇ»
        three
        four
    "});
    cx.update_editor(|e, cx| e.tab(&Tab, cx));
    cx.assert_editor_state(indoc! {"
        \t\t«oneˇ» «twoˇ»
        three
        four
    "});
    cx.update_editor(|e, cx| e.tab_prev(&TabPrev, cx));
    cx.assert_editor_state(indoc! {"
        \t«oneˇ» «twoˇ»
        three
        four
    "});
    cx.update_editor(|e, cx| e.tab_prev(&TabPrev, cx));
    cx.assert_editor_state(indoc! {"
        «oneˇ» «twoˇ»
        three
        four
    "});

    // select across a line ending
    cx.set_state(indoc! {"
        one two
        t«hree
        ˇ»four
    "});
    cx.update_editor(|e, cx| e.tab(&Tab, cx));
    cx.assert_editor_state(indoc! {"
        one two
        \tt«hree
        ˇ»four
    "});
    cx.update_editor(|e, cx| e.tab(&Tab, cx));
    cx.assert_editor_state(indoc! {"
        one two
        \t\tt«hree
        ˇ»four
    "});
    cx.update_editor(|e, cx| e.tab_prev(&TabPrev, cx));
    cx.assert_editor_state(indoc! {"
        one two
        \tt«hree
        ˇ»four
    "});
    cx.update_editor(|e, cx| e.tab_prev(&TabPrev, cx));
    cx.assert_editor_state(indoc! {"
        one two
        t«hree
        ˇ»four
    "});

    // Ensure that indenting/outdenting works when the cursor is at column 0.
    cx.set_state(indoc! {"
        one two
        ˇthree
        four
    "});
    cx.update_editor(|e, cx| e.tab_prev(&TabPrev, cx));
    cx.assert_editor_state(indoc! {"
        one two
        ˇthree
        four
    "});
    cx.update_editor(|e, cx| e.tab(&Tab, cx));
    cx.assert_editor_state(indoc! {"
        one two
        \tˇthree
        four
    "});
    cx.update_editor(|e, cx| e.tab_prev(&TabPrev, cx));
    cx.assert_editor_state(indoc! {"
        one two
        ˇthree
        four
    "});
}

#[gpui::test]
fn test_indent_outdent_with_excerpts(cx: &mut TestAppContext) {
    init_test(cx, |settings| {
        settings.languages.extend([
            (
                "TOML".into(),
                LanguageSettingsContent {
                    tab_size: NonZeroU32::new(2),
                    ..Default::default()
                },
            ),
            (
                "Rust".into(),
                LanguageSettingsContent {
                    tab_size: NonZeroU32::new(4),
                    ..Default::default()
                },
            ),
        ]);
    });

    let toml_language = Arc::new(Language::new(
        LanguageConfig {
            name: "TOML".into(),
            ..Default::default()
        },
        None,
    ));
    let rust_language = Arc::new(Language::new(
        LanguageConfig {
            name: "Rust".into(),
            ..Default::default()
        },
        None,
    ));

    let toml_buffer =
        cx.new_model(|cx| Buffer::local("a = 1\nb = 2\n", cx).with_language(toml_language, cx));
    let rust_buffer = cx.new_model(|cx| {
        Buffer::local("const c: usize = 3;\n", cx).with_language(rust_language, cx)
    });
    let multibuffer = cx.new_model(|cx| {
        let mut multibuffer = MultiBuffer::new(ReadWrite);
        multibuffer.push_excerpts(
            toml_buffer.clone(),
            [ExcerptRange {
                context: Point::new(0, 0)..Point::new(2, 0),
                primary: None,
            }],
            cx,
        );
        multibuffer.push_excerpts(
            rust_buffer.clone(),
            [ExcerptRange {
                context: Point::new(0, 0)..Point::new(1, 0),
                primary: None,
            }],
            cx,
        );
        multibuffer
    });

    cx.add_window(|cx| {
        let mut editor = build_editor(multibuffer, cx);

        assert_eq!(
            editor.text(cx),
            indoc! {"
                a = 1
                b = 2

                const c: usize = 3;
            "}
        );

        select_ranges(
            &mut editor,
            indoc! {"
                «aˇ» = 1
                b = 2

                «const c:ˇ» usize = 3;
            "},
            cx,
        );

        editor.tab(&Tab, cx);
        assert_text_with_selections(
            &mut editor,
            indoc! {"
                  «aˇ» = 1
                b = 2

                    «const c:ˇ» usize = 3;
            "},
            cx,
        );
        editor.tab_prev(&TabPrev, cx);
        assert_text_with_selections(
            &mut editor,
            indoc! {"
                «aˇ» = 1
                b = 2

                «const c:ˇ» usize = 3;
            "},
            cx,
        );

        editor
    });
}

#[gpui::test]
async fn test_backspace(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;

    // Basic backspace
    cx.set_state(indoc! {"
        onˇe two three
        fou«rˇ» five six
        seven «ˇeight nine
        »ten
    "});
    cx.update_editor(|e, cx| e.backspace(&Backspace, cx));
    cx.assert_editor_state(indoc! {"
        oˇe two three
        fouˇ five six
        seven ˇten
    "});

    // Test backspace inside and around indents
    cx.set_state(indoc! {"
        zero
            ˇone
                ˇtwo
            ˇ ˇ ˇ  three
        ˇ  ˇ  four
    "});
    cx.update_editor(|e, cx| e.backspace(&Backspace, cx));
    cx.assert_editor_state(indoc! {"
        zero
        ˇone
            ˇtwo
        ˇ  threeˇ  four
    "});

    // Test backspace with line_mode set to true
    cx.update_editor(|e, _| e.selections.line_mode = true);
    cx.set_state(indoc! {"
        The ˇquick ˇbrown
        fox jumps over
        the lazy dog
        ˇThe qu«ick bˇ»rown"});
    cx.update_editor(|e, cx| e.backspace(&Backspace, cx));
    cx.assert_editor_state(indoc! {"
        ˇfox jumps over
        the lazy dogˇ"});
}

#[gpui::test]
async fn test_delete(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;
    cx.set_state(indoc! {"
        onˇe two three
        fou«rˇ» five six
        seven «ˇeight nine
        »ten
    "});
    cx.update_editor(|e, cx| e.delete(&Delete, cx));
    cx.assert_editor_state(indoc! {"
        onˇ two three
        fouˇ five six
        seven ˇten
    "});

    // Test backspace with line_mode set to true
    cx.update_editor(|e, _| e.selections.line_mode = true);
    cx.set_state(indoc! {"
        The ˇquick ˇbrown
        fox «ˇjum»ps over
        the lazy dog
        ˇThe qu«ick bˇ»rown"});
    cx.update_editor(|e, cx| e.backspace(&Backspace, cx));
    cx.assert_editor_state("ˇthe lazy dogˇ");
}

#[gpui::test]
fn test_delete_line(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let view = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple("abc\ndef\nghi\n", cx);
        build_editor(buffer, cx)
    });
    _ = view.update(cx, |view, cx| {
        view.change_selections(None, cx, |s| {
            s.select_display_ranges([
                DisplayPoint::new(DisplayRow(0), 1)..DisplayPoint::new(DisplayRow(0), 1),
                DisplayPoint::new(DisplayRow(1), 0)..DisplayPoint::new(DisplayRow(1), 1),
                DisplayPoint::new(DisplayRow(3), 0)..DisplayPoint::new(DisplayRow(3), 0),
            ])
        });
        view.delete_line(&DeleteLine, cx);
        assert_eq!(view.display_text(cx), "ghi");
        assert_eq!(
            view.selections.display_ranges(cx),
            vec![
                DisplayPoint::new(DisplayRow(0), 0)..DisplayPoint::new(DisplayRow(0), 0),
                DisplayPoint::new(DisplayRow(0), 1)..DisplayPoint::new(DisplayRow(0), 1)
            ]
        );
    });

    let view = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple("abc\ndef\nghi\n", cx);
        build_editor(buffer, cx)
    });
    _ = view.update(cx, |view, cx| {
        view.change_selections(None, cx, |s| {
            s.select_display_ranges([
                DisplayPoint::new(DisplayRow(2), 0)..DisplayPoint::new(DisplayRow(0), 1)
            ])
        });
        view.delete_line(&DeleteLine, cx);
        assert_eq!(view.display_text(cx), "ghi\n");
        assert_eq!(
            view.selections.display_ranges(cx),
            vec![DisplayPoint::new(DisplayRow(0), 1)..DisplayPoint::new(DisplayRow(0), 1)]
        );
    });
}

#[gpui::test]
fn test_join_lines_with_single_selection(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple("aaa\nbbb\nccc\nddd\n\n", cx);
        let mut editor = build_editor(buffer.clone(), cx);
        let buffer = buffer.read(cx).as_singleton().unwrap();

        assert_eq!(
            editor.selections.ranges::<Point>(cx),
            &[Point::new(0, 0)..Point::new(0, 0)]
        );

        // When on single line, replace newline at end by space
        editor.join_lines(&JoinLines, cx);
        assert_eq!(buffer.read(cx).text(), "aaa bbb\nccc\nddd\n\n");
        assert_eq!(
            editor.selections.ranges::<Point>(cx),
            &[Point::new(0, 3)..Point::new(0, 3)]
        );

        // When multiple lines are selected, remove newlines that are spanned by the selection
        editor.change_selections(None, cx, |s| {
            s.select_ranges([Point::new(0, 5)..Point::new(2, 2)])
        });
        editor.join_lines(&JoinLines, cx);
        assert_eq!(buffer.read(cx).text(), "aaa bbb ccc ddd\n\n");
        assert_eq!(
            editor.selections.ranges::<Point>(cx),
            &[Point::new(0, 11)..Point::new(0, 11)]
        );

        // Undo should be transactional
        editor.undo(&Undo, cx);
        assert_eq!(buffer.read(cx).text(), "aaa bbb\nccc\nddd\n\n");
        assert_eq!(
            editor.selections.ranges::<Point>(cx),
            &[Point::new(0, 5)..Point::new(2, 2)]
        );

        // When joining an empty line don't insert a space
        editor.change_selections(None, cx, |s| {
            s.select_ranges([Point::new(2, 1)..Point::new(2, 2)])
        });
        editor.join_lines(&JoinLines, cx);
        assert_eq!(buffer.read(cx).text(), "aaa bbb\nccc\nddd\n");
        assert_eq!(
            editor.selections.ranges::<Point>(cx),
            [Point::new(2, 3)..Point::new(2, 3)]
        );

        // We can remove trailing newlines
        editor.join_lines(&JoinLines, cx);
        assert_eq!(buffer.read(cx).text(), "aaa bbb\nccc\nddd");
        assert_eq!(
            editor.selections.ranges::<Point>(cx),
            [Point::new(2, 3)..Point::new(2, 3)]
        );

        // We don't blow up on the last line
        editor.join_lines(&JoinLines, cx);
        assert_eq!(buffer.read(cx).text(), "aaa bbb\nccc\nddd");
        assert_eq!(
            editor.selections.ranges::<Point>(cx),
            [Point::new(2, 3)..Point::new(2, 3)]
        );

        // reset to test indentation
        editor.buffer.update(cx, |buffer, cx| {
            buffer.edit(
                [
                    (Point::new(1, 0)..Point::new(1, 2), "  "),
                    (Point::new(2, 0)..Point::new(2, 3), "  \n\td"),
                ],
                None,
                cx,
            )
        });

        // We remove any leading spaces
        assert_eq!(buffer.read(cx).text(), "aaa bbb\n  c\n  \n\td");
        editor.change_selections(None, cx, |s| {
            s.select_ranges([Point::new(0, 1)..Point::new(0, 1)])
        });
        editor.join_lines(&JoinLines, cx);
        assert_eq!(buffer.read(cx).text(), "aaa bbb c\n  \n\td");

        // We don't insert a space for a line containing only spaces
        editor.join_lines(&JoinLines, cx);
        assert_eq!(buffer.read(cx).text(), "aaa bbb c\n\td");

        // We ignore any leading tabs
        editor.join_lines(&JoinLines, cx);
        assert_eq!(buffer.read(cx).text(), "aaa bbb c d");

        editor
    });
}

#[gpui::test]
fn test_join_lines_with_multi_selection(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple("aaa\nbbb\nccc\nddd\n\n", cx);
        let mut editor = build_editor(buffer.clone(), cx);
        let buffer = buffer.read(cx).as_singleton().unwrap();

        editor.change_selections(None, cx, |s| {
            s.select_ranges([
                Point::new(0, 2)..Point::new(1, 1),
                Point::new(1, 2)..Point::new(1, 2),
                Point::new(3, 1)..Point::new(3, 2),
            ])
        });

        editor.join_lines(&JoinLines, cx);
        assert_eq!(buffer.read(cx).text(), "aaa bbb ccc\nddd\n");

        assert_eq!(
            editor.selections.ranges::<Point>(cx),
            [
                Point::new(0, 7)..Point::new(0, 7),
                Point::new(1, 3)..Point::new(1, 3)
            ]
        );
        editor
    });
}

#[gpui::test]
async fn test_join_lines_with_git_diff_base(
    executor: BackgroundExecutor,
    cx: &mut gpui::TestAppContext,
) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;

    let diff_base = r#"
        Line 0
        Line 1
        Line 2
        Line 3
        "#
    .unindent();

    cx.set_state(
        &r#"
        ˇLine 0
        Line 1
        Line 2
        Line 3
        "#
        .unindent(),
    );

    cx.set_diff_base(&diff_base);
    executor.run_until_parked();

    // Join lines
    cx.update_editor(|editor, cx| {
        editor.join_lines(&JoinLines, cx);
    });
    executor.run_until_parked();

    cx.assert_editor_state(
        &r#"
        Line 0ˇ Line 1
        Line 2
        Line 3
        "#
        .unindent(),
    );
    // Join again
    cx.update_editor(|editor, cx| {
        editor.join_lines(&JoinLines, cx);
    });
    executor.run_until_parked();

    cx.assert_editor_state(
        &r#"
        Line 0 Line 1ˇ Line 2
        Line 3
        "#
        .unindent(),
    );
}

#[gpui::test]
async fn test_custom_newlines_cause_no_false_positive_diffs(
    executor: BackgroundExecutor,
    cx: &mut gpui::TestAppContext,
) {
    init_test(cx, |_| {});
    let mut cx = EditorTestContext::new(cx).await;
    cx.set_state("Line 0\r\nLine 1\rˇ\nLine 2\r\nLine 3");
    cx.set_diff_base("Line 0\r\nLine 1\r\nLine 2\r\nLine 3");
    executor.run_until_parked();

    cx.update_editor(|editor, cx| {
        let snapshot = editor.snapshot(cx);
        assert_eq!(
            snapshot
                .diff_map
                .diff_hunks_in_range(0..snapshot.buffer_snapshot.len(), &snapshot.buffer_snapshot)
                .collect::<Vec<_>>(),
            Vec::new(),
            "Should not have any diffs for files with custom newlines"
        );
    });
}

#[gpui::test]
async fn test_manipulate_lines_with_single_selection(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;

    // Test sort_lines_case_insensitive()
    cx.set_state(indoc! {"
        «z
        y
        x
        Z
        Y
        Xˇ»
    "});
    cx.update_editor(|e, cx| e.sort_lines_case_insensitive(&SortLinesCaseInsensitive, cx));
    cx.assert_editor_state(indoc! {"
        «x
        X
        y
        Y
        z
        Zˇ»
    "});

    // Test reverse_lines()
    cx.set_state(indoc! {"
        «5
        4
        3
        2
        1ˇ»
    "});
    cx.update_editor(|e, cx| e.reverse_lines(&ReverseLines, cx));
    cx.assert_editor_state(indoc! {"
        «1
        2
        3
        4
        5ˇ»
    "});

    // Skip testing shuffle_line()

    // From here on out, test more complex cases of manipulate_lines() with a single driver method: sort_lines_case_sensitive()
    // Since all methods calling manipulate_lines() are doing the exact same general thing (reordering lines)

    // Don't manipulate when cursor is on single line, but expand the selection
    cx.set_state(indoc! {"
        ddˇdd
        ccc
        bb
        a
    "});
    cx.update_editor(|e, cx| e.sort_lines_case_sensitive(&SortLinesCaseSensitive, cx));
    cx.assert_editor_state(indoc! {"
        «ddddˇ»
        ccc
        bb
        a
    "});

    // Basic manipulate case
    // Start selection moves to column 0
    // End of selection shrinks to fit shorter line
    cx.set_state(indoc! {"
        dd«d
        ccc
        bb
        aaaaaˇ»
    "});
    cx.update_editor(|e, cx| e.sort_lines_case_sensitive(&SortLinesCaseSensitive, cx));
    cx.assert_editor_state(indoc! {"
        «aaaaa
        bb
        ccc
        dddˇ»
    "});

    // Manipulate case with newlines
    cx.set_state(indoc! {"
        dd«d
        ccc

        bb
        aaaaa

        ˇ»
    "});
    cx.update_editor(|e, cx| e.sort_lines_case_sensitive(&SortLinesCaseSensitive, cx));
    cx.assert_editor_state(indoc! {"
        «

        aaaaa
        bb
        ccc
        dddˇ»

    "});

    // Adding new line
    cx.set_state(indoc! {"
        aa«a
        bbˇ»b
    "});
    cx.update_editor(|e, cx| e.manipulate_lines(cx, |lines| lines.push("added_line")));
    cx.assert_editor_state(indoc! {"
        «aaa
        bbb
        added_lineˇ»
    "});

    // Removing line
    cx.set_state(indoc! {"
        aa«a
        bbbˇ»
    "});
    cx.update_editor(|e, cx| {
        e.manipulate_lines(cx, |lines| {
            lines.pop();
        })
    });
    cx.assert_editor_state(indoc! {"
        «aaaˇ»
    "});

    // Removing all lines
    cx.set_state(indoc! {"
        aa«a
        bbbˇ»
    "});
    cx.update_editor(|e, cx| {
        e.manipulate_lines(cx, |lines| {
            lines.drain(..);
        })
    });
    cx.assert_editor_state(indoc! {"
        ˇ
    "});
}

#[gpui::test]
async fn test_unique_lines_multi_selection(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;

    // Consider continuous selection as single selection
    cx.set_state(indoc! {"
        Aaa«aa
        cˇ»c«c
        bb
        aaaˇ»aa
    "});
    cx.update_editor(|e, cx| e.unique_lines_case_sensitive(&UniqueLinesCaseSensitive, cx));
    cx.assert_editor_state(indoc! {"
        «Aaaaa
        ccc
        bb
        aaaaaˇ»
    "});

    cx.set_state(indoc! {"
        Aaa«aa
        cˇ»c«c
        bb
        aaaˇ»aa
    "});
    cx.update_editor(|e, cx| e.unique_lines_case_insensitive(&UniqueLinesCaseInsensitive, cx));
    cx.assert_editor_state(indoc! {"
        «Aaaaa
        ccc
        bbˇ»
    "});

    // Consider non continuous selection as distinct dedup operations
    cx.set_state(indoc! {"
        «aaaaa
        bb
        aaaaa
        aaaaaˇ»

        aaa«aaˇ»
    "});
    cx.update_editor(|e, cx| e.unique_lines_case_sensitive(&UniqueLinesCaseSensitive, cx));
    cx.assert_editor_state(indoc! {"
        «aaaaa
        bbˇ»

        «aaaaaˇ»
    "});
}

#[gpui::test]
async fn test_unique_lines_single_selection(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;

    cx.set_state(indoc! {"
        «Aaa
        aAa
        Aaaˇ»
    "});
    cx.update_editor(|e, cx| e.unique_lines_case_sensitive(&UniqueLinesCaseSensitive, cx));
    cx.assert_editor_state(indoc! {"
        «Aaa
        aAaˇ»
    "});

    cx.set_state(indoc! {"
        «Aaa
        aAa
        aaAˇ»
    "});
    cx.update_editor(|e, cx| e.unique_lines_case_insensitive(&UniqueLinesCaseInsensitive, cx));
    cx.assert_editor_state(indoc! {"
        «Aaaˇ»
    "});
}

#[gpui::test]
async fn test_manipulate_lines_with_multi_selection(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;

    // Manipulate with multiple selections on a single line
    cx.set_state(indoc! {"
        dd«dd
        cˇ»c«c
        bb
        aaaˇ»aa
    "});
    cx.update_editor(|e, cx| e.sort_lines_case_sensitive(&SortLinesCaseSensitive, cx));
    cx.assert_editor_state(indoc! {"
        «aaaaa
        bb
        ccc
        ddddˇ»
    "});

    // Manipulate with multiple disjoin selections
    cx.set_state(indoc! {"
        5«
        4
        3
        2
        1ˇ»

        dd«dd
        ccc
        bb
        aaaˇ»aa
    "});
    cx.update_editor(|e, cx| e.sort_lines_case_sensitive(&SortLinesCaseSensitive, cx));
    cx.assert_editor_state(indoc! {"
        «1
        2
        3
        4
        5ˇ»

        «aaaaa
        bb
        ccc
        ddddˇ»
    "});

    // Adding lines on each selection
    cx.set_state(indoc! {"
        2«
        1ˇ»

        bb«bb
        aaaˇ»aa
    "});
    cx.update_editor(|e, cx| e.manipulate_lines(cx, |lines| lines.push("added line")));
    cx.assert_editor_state(indoc! {"
        «2
        1
        added lineˇ»

        «bbbb
        aaaaa
        added lineˇ»
    "});

    // Removing lines on each selection
    cx.set_state(indoc! {"
        2«
        1ˇ»

        bb«bb
        aaaˇ»aa
    "});
    cx.update_editor(|e, cx| {
        e.manipulate_lines(cx, |lines| {
            lines.pop();
        })
    });
    cx.assert_editor_state(indoc! {"
        «2ˇ»

        «bbbbˇ»
    "});
}

#[gpui::test]
async fn test_manipulate_text(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;

    // Test convert_to_upper_case()
    cx.set_state(indoc! {"
        «hello worldˇ»
    "});
    cx.update_editor(|e, cx| e.convert_to_upper_case(&ConvertToUpperCase, cx));
    cx.assert_editor_state(indoc! {"
        «HELLO WORLDˇ»
    "});

    // Test convert_to_lower_case()
    cx.set_state(indoc! {"
        «HELLO WORLDˇ»
    "});
    cx.update_editor(|e, cx| e.convert_to_lower_case(&ConvertToLowerCase, cx));
    cx.assert_editor_state(indoc! {"
        «hello worldˇ»
    "});

    // Test multiple line, single selection case
    // Test code hack that covers the fact that to_case crate doesn't support '\n' as a word boundary
    cx.set_state(indoc! {"
        «The quick brown
        fox jumps over
        the lazy dogˇ»
    "});
    cx.update_editor(|e, cx| e.convert_to_title_case(&ConvertToTitleCase, cx));
    cx.assert_editor_state(indoc! {"
        «The Quick Brown
        Fox Jumps Over
        The Lazy Dogˇ»
    "});

    // Test multiple line, single selection case
    // Test code hack that covers the fact that to_case crate doesn't support '\n' as a word boundary
    cx.set_state(indoc! {"
        «The quick brown
        fox jumps over
        the lazy dogˇ»
    "});
    cx.update_editor(|e, cx| e.convert_to_upper_camel_case(&ConvertToUpperCamelCase, cx));
    cx.assert_editor_state(indoc! {"
        «TheQuickBrown
        FoxJumpsOver
        TheLazyDogˇ»
    "});

    // From here on out, test more complex cases of manipulate_text()

    // Test no selection case - should affect words cursors are in
    // Cursor at beginning, middle, and end of word
    cx.set_state(indoc! {"
        ˇhello big beauˇtiful worldˇ
    "});
    cx.update_editor(|e, cx| e.convert_to_upper_case(&ConvertToUpperCase, cx));
    cx.assert_editor_state(indoc! {"
        «HELLOˇ» big «BEAUTIFULˇ» «WORLDˇ»
    "});

    // Test multiple selections on a single line and across multiple lines
    cx.set_state(indoc! {"
        «Theˇ» quick «brown
        foxˇ» jumps «overˇ»
        the «lazyˇ» dog
    "});
    cx.update_editor(|e, cx| e.convert_to_upper_case(&ConvertToUpperCase, cx));
    cx.assert_editor_state(indoc! {"
        «THEˇ» quick «BROWN
        FOXˇ» jumps «OVERˇ»
        the «LAZYˇ» dog
    "});

    // Test case where text length grows
    cx.set_state(indoc! {"
        «tschüßˇ»
    "});
    cx.update_editor(|e, cx| e.convert_to_upper_case(&ConvertToUpperCase, cx));
    cx.assert_editor_state(indoc! {"
        «TSCHÜSSˇ»
    "});

    // Test to make sure we don't crash when text shrinks
    cx.set_state(indoc! {"
        aaa_bbbˇ
    "});
    cx.update_editor(|e, cx| e.convert_to_lower_camel_case(&ConvertToLowerCamelCase, cx));
    cx.assert_editor_state(indoc! {"
        «aaaBbbˇ»
    "});

    // Test to make sure we all aware of the fact that each word can grow and shrink
    // Final selections should be aware of this fact
    cx.set_state(indoc! {"
        aaa_bˇbb bbˇb_ccc ˇccc_ddd
    "});
    cx.update_editor(|e, cx| e.convert_to_lower_camel_case(&ConvertToLowerCamelCase, cx));
    cx.assert_editor_state(indoc! {"
        «aaaBbbˇ» «bbbCccˇ» «cccDddˇ»
    "});

    cx.set_state(indoc! {"
        «hElLo, WoRld!ˇ»
    "});
    cx.update_editor(|e, cx| e.convert_to_opposite_case(&ConvertToOppositeCase, cx));
    cx.assert_editor_state(indoc! {"
        «HeLlO, wOrLD!ˇ»
    "});
}

#[gpui::test]
fn test_duplicate_line(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let view = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple("abc\ndef\nghi\n", cx);
        build_editor(buffer, cx)
    });
    _ = view.update(cx, |view, cx| {
        view.change_selections(None, cx, |s| {
            s.select_display_ranges([
                DisplayPoint::new(DisplayRow(0), 0)..DisplayPoint::new(DisplayRow(0), 1),
                DisplayPoint::new(DisplayRow(0), 2)..DisplayPoint::new(DisplayRow(0), 2),
                DisplayPoint::new(DisplayRow(1), 0)..DisplayPoint::new(DisplayRow(1), 0),
                DisplayPoint::new(DisplayRow(3), 0)..DisplayPoint::new(DisplayRow(3), 0),
            ])
        });
        view.duplicate_line_down(&DuplicateLineDown, cx);
        assert_eq!(view.display_text(cx), "abc\nabc\ndef\ndef\nghi\n\n");
        assert_eq!(
            view.selections.display_ranges(cx),
            vec![
                DisplayPoint::new(DisplayRow(1), 0)..DisplayPoint::new(DisplayRow(1), 1),
                DisplayPoint::new(DisplayRow(1), 2)..DisplayPoint::new(DisplayRow(1), 2),
                DisplayPoint::new(DisplayRow(3), 0)..DisplayPoint::new(DisplayRow(3), 0),
                DisplayPoint::new(DisplayRow(6), 0)..DisplayPoint::new(DisplayRow(6), 0),
            ]
        );
    });

    let view = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple("abc\ndef\nghi\n", cx);
        build_editor(buffer, cx)
    });
    _ = view.update(cx, |view, cx| {
        view.change_selections(None, cx, |s| {
            s.select_display_ranges([
                DisplayPoint::new(DisplayRow(0), 1)..DisplayPoint::new(DisplayRow(1), 1),
                DisplayPoint::new(DisplayRow(1), 2)..DisplayPoint::new(DisplayRow(2), 1),
            ])
        });
        view.duplicate_line_down(&DuplicateLineDown, cx);
        assert_eq!(view.display_text(cx), "abc\ndef\nghi\nabc\ndef\nghi\n");
        assert_eq!(
            view.selections.display_ranges(cx),
            vec![
                DisplayPoint::new(DisplayRow(3), 1)..DisplayPoint::new(DisplayRow(4), 1),
                DisplayPoint::new(DisplayRow(4), 2)..DisplayPoint::new(DisplayRow(5), 1),
            ]
        );
    });

    // With `move_upwards` the selections stay in place, except for
    // the lines inserted above them
    let view = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple("abc\ndef\nghi\n", cx);
        build_editor(buffer, cx)
    });
    _ = view.update(cx, |view, cx| {
        view.change_selections(None, cx, |s| {
            s.select_display_ranges([
                DisplayPoint::new(DisplayRow(0), 0)..DisplayPoint::new(DisplayRow(0), 1),
                DisplayPoint::new(DisplayRow(0), 2)..DisplayPoint::new(DisplayRow(0), 2),
                DisplayPoint::new(DisplayRow(1), 0)..DisplayPoint::new(DisplayRow(1), 0),
                DisplayPoint::new(DisplayRow(3), 0)..DisplayPoint::new(DisplayRow(3), 0),
            ])
        });
        view.duplicate_line_up(&DuplicateLineUp, cx);
        assert_eq!(view.display_text(cx), "abc\nabc\ndef\ndef\nghi\n\n");
        assert_eq!(
            view.selections.display_ranges(cx),
            vec![
                DisplayPoint::new(DisplayRow(0), 0)..DisplayPoint::new(DisplayRow(0), 1),
                DisplayPoint::new(DisplayRow(0), 2)..DisplayPoint::new(DisplayRow(0), 2),
                DisplayPoint::new(DisplayRow(2), 0)..DisplayPoint::new(DisplayRow(2), 0),
                DisplayPoint::new(DisplayRow(6), 0)..DisplayPoint::new(DisplayRow(6), 0),
            ]
        );
    });

    let view = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple("abc\ndef\nghi\n", cx);
        build_editor(buffer, cx)
    });
    _ = view.update(cx, |view, cx| {
        view.change_selections(None, cx, |s| {
            s.select_display_ranges([
                DisplayPoint::new(DisplayRow(0), 1)..DisplayPoint::new(DisplayRow(1), 1),
                DisplayPoint::new(DisplayRow(1), 2)..DisplayPoint::new(DisplayRow(2), 1),
            ])
        });
        view.duplicate_line_up(&DuplicateLineUp, cx);
        assert_eq!(view.display_text(cx), "abc\ndef\nghi\nabc\ndef\nghi\n");
        assert_eq!(
            view.selections.display_ranges(cx),
            vec![
                DisplayPoint::new(DisplayRow(0), 1)..DisplayPoint::new(DisplayRow(1), 1),
                DisplayPoint::new(DisplayRow(1), 2)..DisplayPoint::new(DisplayRow(2), 1),
            ]
        );
    });

    let view = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple("abc\ndef\nghi\n", cx);
        build_editor(buffer, cx)
    });
    _ = view.update(cx, |view, cx| {
        view.change_selections(None, cx, |s| {
            s.select_display_ranges([
                DisplayPoint::new(DisplayRow(0), 1)..DisplayPoint::new(DisplayRow(1), 1),
                DisplayPoint::new(DisplayRow(1), 2)..DisplayPoint::new(DisplayRow(2), 1),
            ])
        });
        view.duplicate_selection(&DuplicateSelection, cx);
        assert_eq!(view.display_text(cx), "abc\ndbc\ndef\ngf\nghi\n");
        assert_eq!(
            view.selections.display_ranges(cx),
            vec![
                DisplayPoint::new(DisplayRow(0), 1)..DisplayPoint::new(DisplayRow(1), 1),
                DisplayPoint::new(DisplayRow(2), 2)..DisplayPoint::new(DisplayRow(3), 1),
            ]
        );
    });
}

#[gpui::test]
fn test_move_line_up_down(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let view = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple(&sample_text(10, 5, 'a'), cx);
        build_editor(buffer, cx)
    });
    _ = view.update(cx, |view, cx| {
        view.fold_creases(
            vec![
                Crease::simple(Point::new(0, 2)..Point::new(1, 2), FoldPlaceholder::test()),
                Crease::simple(Point::new(2, 3)..Point::new(4, 1), FoldPlaceholder::test()),
                Crease::simple(Point::new(7, 0)..Point::new(8, 4), FoldPlaceholder::test()),
            ],
            true,
            cx,
        );
        view.change_selections(None, cx, |s| {
            s.select_display_ranges([
                DisplayPoint::new(DisplayRow(0), 1)..DisplayPoint::new(DisplayRow(0), 1),
                DisplayPoint::new(DisplayRow(3), 1)..DisplayPoint::new(DisplayRow(3), 1),
                DisplayPoint::new(DisplayRow(3), 2)..DisplayPoint::new(DisplayRow(4), 3),
                DisplayPoint::new(DisplayRow(5), 0)..DisplayPoint::new(DisplayRow(5), 2),
            ])
        });
        assert_eq!(
            view.display_text(cx),
            "aa⋯bbb\nccc⋯eeee\nfffff\nggggg\n⋯i\njjjjj"
        );

        view.move_line_up(&MoveLineUp, cx);
        assert_eq!(
            view.display_text(cx),
            "aa⋯bbb\nccc⋯eeee\nggggg\n⋯i\njjjjj\nfffff"
        );
        assert_eq!(
            view.selections.display_ranges(cx),
            vec![
                DisplayPoint::new(DisplayRow(0), 1)..DisplayPoint::new(DisplayRow(0), 1),
                DisplayPoint::new(DisplayRow(2), 1)..DisplayPoint::new(DisplayRow(2), 1),
                DisplayPoint::new(DisplayRow(2), 2)..DisplayPoint::new(DisplayRow(3), 3),
                DisplayPoint::new(DisplayRow(4), 0)..DisplayPoint::new(DisplayRow(4), 2)
            ]
        );
    });

    _ = view.update(cx, |view, cx| {
        view.move_line_down(&MoveLineDown, cx);
        assert_eq!(
            view.display_text(cx),
            "ccc⋯eeee\naa⋯bbb\nfffff\nggggg\n⋯i\njjjjj"
        );
        assert_eq!(
            view.selections.display_ranges(cx),
            vec![
                DisplayPoint::new(DisplayRow(1), 1)..DisplayPoint::new(DisplayRow(1), 1),
                DisplayPoint::new(DisplayRow(3), 1)..DisplayPoint::new(DisplayRow(3), 1),
                DisplayPoint::new(DisplayRow(3), 2)..DisplayPoint::new(DisplayRow(4), 3),
                DisplayPoint::new(DisplayRow(5), 0)..DisplayPoint::new(DisplayRow(5), 2)
            ]
        );
    });

    _ = view.update(cx, |view, cx| {
        view.move_line_down(&MoveLineDown, cx);
        assert_eq!(
            view.display_text(cx),
            "ccc⋯eeee\nfffff\naa⋯bbb\nggggg\n⋯i\njjjjj"
        );
        assert_eq!(
            view.selections.display_ranges(cx),
            vec![
                DisplayPoint::new(DisplayRow(2), 1)..DisplayPoint::new(DisplayRow(2), 1),
                DisplayPoint::new(DisplayRow(3), 1)..DisplayPoint::new(DisplayRow(3), 1),
                DisplayPoint::new(DisplayRow(3), 2)..DisplayPoint::new(DisplayRow(4), 3),
                DisplayPoint::new(DisplayRow(5), 0)..DisplayPoint::new(DisplayRow(5), 2)
            ]
        );
    });

    _ = view.update(cx, |view, cx| {
        view.move_line_up(&MoveLineUp, cx);
        assert_eq!(
            view.display_text(cx),
            "ccc⋯eeee\naa⋯bbb\nggggg\n⋯i\njjjjj\nfffff"
        );
        assert_eq!(
            view.selections.display_ranges(cx),
            vec![
                DisplayPoint::new(DisplayRow(1), 1)..DisplayPoint::new(DisplayRow(1), 1),
                DisplayPoint::new(DisplayRow(2), 1)..DisplayPoint::new(DisplayRow(2), 1),
                DisplayPoint::new(DisplayRow(2), 2)..DisplayPoint::new(DisplayRow(3), 3),
                DisplayPoint::new(DisplayRow(4), 0)..DisplayPoint::new(DisplayRow(4), 2)
            ]
        );
    });
}

#[gpui::test]
fn test_move_line_up_down_with_blocks(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let editor = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple(&sample_text(10, 5, 'a'), cx);
        build_editor(buffer, cx)
    });
    _ = editor.update(cx, |editor, cx| {
        let snapshot = editor.buffer.read(cx).snapshot(cx);
        editor.insert_blocks(
            [BlockProperties {
                style: BlockStyle::Fixed,
                placement: BlockPlacement::Below(snapshot.anchor_after(Point::new(2, 0))),
                height: 1,
                render: Arc::new(|_| div().into_any()),
                priority: 0,
            }],
            Some(Autoscroll::fit()),
            cx,
        );
        editor.change_selections(None, cx, |s| {
            s.select_ranges([Point::new(2, 0)..Point::new(2, 0)])
        });
        editor.move_line_down(&MoveLineDown, cx);
    });
}

#[gpui::test]
async fn test_selections_and_replace_blocks(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;
    cx.set_state(
        &"
            ˇzero
            one
            two
            three
            four
            five
        "
        .unindent(),
    );

    // Create a four-line block that replaces three lines of text.
    cx.update_editor(|editor, cx| {
        let snapshot = editor.snapshot(cx);
        let snapshot = &snapshot.buffer_snapshot;
        let placement = BlockPlacement::Replace(
            snapshot.anchor_after(Point::new(1, 0))..=snapshot.anchor_after(Point::new(3, 0)),
        );
        editor.insert_blocks(
            [BlockProperties {
                placement,
                height: 4,
                style: BlockStyle::Sticky,
                render: Arc::new(|_| gpui::div().into_any_element()),
                priority: 0,
            }],
            None,
            cx,
        );
    });

    // Move down so that the cursor touches the block.
    cx.update_editor(|editor, cx| {
        editor.move_down(&Default::default(), cx);
    });
    cx.assert_editor_state(
        &"
            zero
            «one
            two
            threeˇ»
            four
            five
        "
        .unindent(),
    );

    // Move down past the block.
    cx.update_editor(|editor, cx| {
        editor.move_down(&Default::default(), cx);
    });
    cx.assert_editor_state(
        &"
            zero
            one
            two
            three
            ˇfour
            five
        "
        .unindent(),
    );
}

#[gpui::test]
fn test_transpose(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    _ = cx.add_window(|cx| {
        let mut editor = build_editor(MultiBuffer::build_simple("abc", cx), cx);
        editor.set_style(EditorStyle::default(), cx);
        editor.change_selections(None, cx, |s| s.select_ranges([1..1]));
        editor.transpose(&Default::default(), cx);
        assert_eq!(editor.text(cx), "bac");
        assert_eq!(editor.selections.ranges(cx), [2..2]);

        editor.transpose(&Default::default(), cx);
        assert_eq!(editor.text(cx), "bca");
        assert_eq!(editor.selections.ranges(cx), [3..3]);

        editor.transpose(&Default::default(), cx);
        assert_eq!(editor.text(cx), "bac");
        assert_eq!(editor.selections.ranges(cx), [3..3]);

        editor
    });

    _ = cx.add_window(|cx| {
        let mut editor = build_editor(MultiBuffer::build_simple("abc\nde", cx), cx);
        editor.set_style(EditorStyle::default(), cx);
        editor.change_selections(None, cx, |s| s.select_ranges([3..3]));
        editor.transpose(&Default::default(), cx);
        assert_eq!(editor.text(cx), "acb\nde");
        assert_eq!(editor.selections.ranges(cx), [3..3]);

        editor.change_selections(None, cx, |s| s.select_ranges([4..4]));
        editor.transpose(&Default::default(), cx);
        assert_eq!(editor.text(cx), "acbd\ne");
        assert_eq!(editor.selections.ranges(cx), [5..5]);

        editor.transpose(&Default::default(), cx);
        assert_eq!(editor.text(cx), "acbde\n");
        assert_eq!(editor.selections.ranges(cx), [6..6]);

        editor.transpose(&Default::default(), cx);
        assert_eq!(editor.text(cx), "acbd\ne");
        assert_eq!(editor.selections.ranges(cx), [6..6]);

        editor
    });

    _ = cx.add_window(|cx| {
        let mut editor = build_editor(MultiBuffer::build_simple("abc\nde", cx), cx);
        editor.set_style(EditorStyle::default(), cx);
        editor.change_selections(None, cx, |s| s.select_ranges([1..1, 2..2, 4..4]));
        editor.transpose(&Default::default(), cx);
        assert_eq!(editor.text(cx), "bacd\ne");
        assert_eq!(editor.selections.ranges(cx), [2..2, 3..3, 5..5]);

        editor.transpose(&Default::default(), cx);
        assert_eq!(editor.text(cx), "bcade\n");
        assert_eq!(editor.selections.ranges(cx), [3..3, 4..4, 6..6]);

        editor.transpose(&Default::default(), cx);
        assert_eq!(editor.text(cx), "bcda\ne");
        assert_eq!(editor.selections.ranges(cx), [4..4, 6..6]);

        editor.transpose(&Default::default(), cx);
        assert_eq!(editor.text(cx), "bcade\n");
        assert_eq!(editor.selections.ranges(cx), [4..4, 6..6]);

        editor.transpose(&Default::default(), cx);
        assert_eq!(editor.text(cx), "bcaed\n");
        assert_eq!(editor.selections.ranges(cx), [5..5, 6..6]);

        editor
    });

    _ = cx.add_window(|cx| {
        let mut editor = build_editor(MultiBuffer::build_simple("🍐🏀✋", cx), cx);
        editor.set_style(EditorStyle::default(), cx);
        editor.change_selections(None, cx, |s| s.select_ranges([4..4]));
        editor.transpose(&Default::default(), cx);
        assert_eq!(editor.text(cx), "🏀🍐✋");
        assert_eq!(editor.selections.ranges(cx), [8..8]);

        editor.transpose(&Default::default(), cx);
        assert_eq!(editor.text(cx), "🏀✋🍐");
        assert_eq!(editor.selections.ranges(cx), [11..11]);

        editor.transpose(&Default::default(), cx);
        assert_eq!(editor.text(cx), "🏀🍐✋");
        assert_eq!(editor.selections.ranges(cx), [11..11]);

        editor
    });
}

#[gpui::test]
async fn test_rewrap(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;

    let language_with_c_comments = Arc::new(Language::new(
        LanguageConfig {
            line_comments: vec!["// ".into()],
            ..LanguageConfig::default()
        },
        None,
    ));
    let language_with_pound_comments = Arc::new(Language::new(
        LanguageConfig {
            line_comments: vec!["# ".into()],
            ..LanguageConfig::default()
        },
        None,
    ));
    let markdown_language = Arc::new(Language::new(
        LanguageConfig {
            name: "Markdown".into(),
            ..LanguageConfig::default()
        },
        None,
    ));
    let language_with_doc_comments = Arc::new(Language::new(
        LanguageConfig {
            line_comments: vec!["// ".into(), "/// ".into()],
            ..LanguageConfig::default()
        },
        Some(tree_sitter_rust::LANGUAGE.into()),
    ));

    let plaintext_language = Arc::new(Language::new(
        LanguageConfig {
            name: "Plain Text".into(),
            ..LanguageConfig::default()
        },
        None,
    ));

    assert_rewrap(
        indoc! {"
            // ˇLorem ipsum dolor sit amet, consectetur adipiscing elit. Vivamus mollis elit purus, a ornare lacus gravida vitae. Proin consectetur felis vel purus auctor, eu lacinia sapien scelerisque. Vivamus sit amet neque et quam tincidunt hendrerit. Praesent semper egestas tellus id dignissim. Pellentesque odio lectus, iaculis ac volutpat et, blandit quis urna. Sed vestibulum nisi sit amet nisl venenatis tempus. Donec molestie blandit quam, et porta nunc laoreet in. Integer sit amet scelerisque nisi. Lorem ipsum dolor sit amet, consectetur adipiscing elit. Cras egestas porta metus, eu viverra ipsum efficitur quis. Donec luctus eros turpis, id vulputate turpis porttitor id. Aliquam id accumsan eros.
        "},
        indoc! {"
            // ˇLorem ipsum dolor sit amet, consectetur adipiscing elit. Vivamus mollis elit
            // purus, a ornare lacus gravida vitae. Proin consectetur felis vel purus
            // auctor, eu lacinia sapien scelerisque. Vivamus sit amet neque et quam
            // tincidunt hendrerit. Praesent semper egestas tellus id dignissim.
            // Pellentesque odio lectus, iaculis ac volutpat et, blandit quis urna. Sed
            // vestibulum nisi sit amet nisl venenatis tempus. Donec molestie blandit quam,
            // et porta nunc laoreet in. Integer sit amet scelerisque nisi. Lorem ipsum
            // dolor sit amet, consectetur adipiscing elit. Cras egestas porta metus, eu
            // viverra ipsum efficitur quis. Donec luctus eros turpis, id vulputate turpis
            // porttitor id. Aliquam id accumsan eros.
        "},
        language_with_c_comments.clone(),
        &mut cx,
    );

    // Test that rewrapping works inside of a selection
    assert_rewrap(
        indoc! {"
            «// Lorem ipsum dolor sit amet, consectetur adipiscing elit. Vivamus mollis elit purus, a ornare lacus gravida vitae. Proin consectetur felis vel purus auctor, eu lacinia sapien scelerisque. Vivamus sit amet neque et quam tincidunt hendrerit. Praesent semper egestas tellus id dignissim. Pellentesque odio lectus, iaculis ac volutpat et, blandit quis urna. Sed vestibulum nisi sit amet nisl venenatis tempus. Donec molestie blandit quam, et porta nunc laoreet in. Integer sit amet scelerisque nisi. Lorem ipsum dolor sit amet, consectetur adipiscing elit. Cras egestas porta metus, eu viverra ipsum efficitur quis. Donec luctus eros turpis, id vulputate turpis porttitor id. Aliquam id accumsan eros.ˇ»
        "},
        indoc! {"
            «// Lorem ipsum dolor sit amet, consectetur adipiscing elit. Vivamus mollis elit
            // purus, a ornare lacus gravida vitae. Proin consectetur felis vel purus
            // auctor, eu lacinia sapien scelerisque. Vivamus sit amet neque et quam
            // tincidunt hendrerit. Praesent semper egestas tellus id dignissim.
            // Pellentesque odio lectus, iaculis ac volutpat et, blandit quis urna. Sed
            // vestibulum nisi sit amet nisl venenatis tempus. Donec molestie blandit quam,
            // et porta nunc laoreet in. Integer sit amet scelerisque nisi. Lorem ipsum
            // dolor sit amet, consectetur adipiscing elit. Cras egestas porta metus, eu
            // viverra ipsum efficitur quis. Donec luctus eros turpis, id vulputate turpis
            // porttitor id. Aliquam id accumsan eros.ˇ»
        "},
        language_with_c_comments.clone(),
        &mut cx,
    );

    // Test that cursors that expand to the same region are collapsed.
    assert_rewrap(
        indoc! {"
            // ˇLorem ipsum dolor sit amet, consectetur adipiscing elit.
            // ˇVivamus mollis elit purus, a ornare lacus gravida vitae. Proin consectetur felis vel purus auctor, eu lacinia sapien scelerisque.
            // ˇVivamus sit amet neque et quam tincidunt hendrerit. Praesent semper egestas tellus id dignissim. Pellentesque odio lectus, iaculis ac volutpat et,
            // ˇblandit quis urna. Sed vestibulum nisi sit amet nisl venenatis tempus. Donec molestie blandit quam, et porta nunc laoreet in. Integer sit amet scelerisque nisi. Lorem ipsum dolor sit amet, consectetur adipiscing elit. Cras egestas porta metus, eu viverra ipsum efficitur quis. Donec luctus eros turpis, id vulputate turpis porttitor id. Aliquam id accumsan eros.
        "},
        indoc! {"
            // ˇLorem ipsum dolor sit amet, consectetur adipiscing elit. ˇVivamus mollis elit
            // purus, a ornare lacus gravida vitae. Proin consectetur felis vel purus
            // auctor, eu lacinia sapien scelerisque. ˇVivamus sit amet neque et quam
            // tincidunt hendrerit. Praesent semper egestas tellus id dignissim.
            // Pellentesque odio lectus, iaculis ac volutpat et, ˇblandit quis urna. Sed
            // vestibulum nisi sit amet nisl venenatis tempus. Donec molestie blandit quam,
            // et porta nunc laoreet in. Integer sit amet scelerisque nisi. Lorem ipsum
            // dolor sit amet, consectetur adipiscing elit. Cras egestas porta metus, eu
            // viverra ipsum efficitur quis. Donec luctus eros turpis, id vulputate turpis
            // porttitor id. Aliquam id accumsan eros.
        "},
        language_with_c_comments.clone(),
        &mut cx,
    );

    // Test that non-contiguous selections are treated separately.
    assert_rewrap(
        indoc! {"
            // ˇLorem ipsum dolor sit amet, consectetur adipiscing elit.
            // ˇVivamus mollis elit purus, a ornare lacus gravida vitae. Proin consectetur felis vel purus auctor, eu lacinia sapien scelerisque.
            //
            // ˇVivamus sit amet neque et quam tincidunt hendrerit. Praesent semper egestas tellus id dignissim. Pellentesque odio lectus, iaculis ac volutpat et,
            // ˇblandit quis urna. Sed vestibulum nisi sit amet nisl venenatis tempus. Donec molestie blandit quam, et porta nunc laoreet in. Integer sit amet scelerisque nisi. Lorem ipsum dolor sit amet, consectetur adipiscing elit. Cras egestas porta metus, eu viverra ipsum efficitur quis. Donec luctus eros turpis, id vulputate turpis porttitor id. Aliquam id accumsan eros.
        "},
        indoc! {"
            // ˇLorem ipsum dolor sit amet, consectetur adipiscing elit. ˇVivamus mollis elit
            // purus, a ornare lacus gravida vitae. Proin consectetur felis vel purus
            // auctor, eu lacinia sapien scelerisque.
            //
            // ˇVivamus sit amet neque et quam tincidunt hendrerit. Praesent semper egestas
            // tellus id dignissim. Pellentesque odio lectus, iaculis ac volutpat et,
            // ˇblandit quis urna. Sed vestibulum nisi sit amet nisl venenatis tempus. Donec
            // molestie blandit quam, et porta nunc laoreet in. Integer sit amet scelerisque
            // nisi. Lorem ipsum dolor sit amet, consectetur adipiscing elit. Cras egestas
            // porta metus, eu viverra ipsum efficitur quis. Donec luctus eros turpis, id
            // vulputate turpis porttitor id. Aliquam id accumsan eros.
        "},
        language_with_c_comments.clone(),
        &mut cx,
    );

    // Test that different comment prefixes are supported.
    assert_rewrap(
        indoc! {"
            # ˇLorem ipsum dolor sit amet, consectetur adipiscing elit. Vivamus mollis elit purus, a ornare lacus gravida vitae. Proin consectetur felis vel purus auctor, eu lacinia sapien scelerisque. Vivamus sit amet neque et quam tincidunt hendrerit. Praesent semper egestas tellus id dignissim. Pellentesque odio lectus, iaculis ac volutpat et, blandit quis urna. Sed vestibulum nisi sit amet nisl venenatis tempus. Donec molestie blandit quam, et porta nunc laoreet in. Integer sit amet scelerisque nisi. Lorem ipsum dolor sit amet, consectetur adipiscing elit. Cras egestas porta metus, eu viverra ipsum efficitur quis. Donec luctus eros turpis, id vulputate turpis porttitor id. Aliquam id accumsan eros.
        "},
        indoc! {"
            # ˇLorem ipsum dolor sit amet, consectetur adipiscing elit. Vivamus mollis elit
            # purus, a ornare lacus gravida vitae. Proin consectetur felis vel purus auctor,
            # eu lacinia sapien scelerisque. Vivamus sit amet neque et quam tincidunt
            # hendrerit. Praesent semper egestas tellus id dignissim. Pellentesque odio
            # lectus, iaculis ac volutpat et, blandit quis urna. Sed vestibulum nisi sit
            # amet nisl venenatis tempus. Donec molestie blandit quam, et porta nunc laoreet
            # in. Integer sit amet scelerisque nisi. Lorem ipsum dolor sit amet, consectetur
            # adipiscing elit. Cras egestas porta metus, eu viverra ipsum efficitur quis.
            # Donec luctus eros turpis, id vulputate turpis porttitor id. Aliquam id
            # accumsan eros.
        "},
        language_with_pound_comments.clone(),
        &mut cx,
    );

    // Test that rewrapping is ignored outside of comments in most languages.
    assert_rewrap(
        indoc! {"
            /// Adds two numbers.
            /// Lorem ipsum dolor sit amet, consectetur adipiscing elit. Vivamus mollis elit purus, a ornare lacus gravida vitae.ˇ
            fn add(a: u32, b: u32) -> u32 {
                a + b + a + b + a + b + a + b + a + b + a + b + a + b + a + b + a + b + a + b + a + b + a + b + a + b + a + b + a + b + a + bˇ
            }
        "},
        indoc! {"
            /// Adds two numbers. Lorem ipsum dolor sit amet, consectetur adipiscing elit.
            /// Vivamus mollis elit purus, a ornare lacus gravida vitae.ˇ
            fn add(a: u32, b: u32) -> u32 {
                a + b + a + b + a + b + a + b + a + b + a + b + a + b + a + b + a + b + a + b + a + b + a + b + a + b + a + b + a + b + a + bˇ
            }
        "},
        language_with_doc_comments.clone(),
        &mut cx,
    );

    // Test that rewrapping works in Markdown and Plain Text languages.
    assert_rewrap(
        indoc! {"
            # Hello

            Lorem ipsum dolor sit amet, ˇconsectetur adipiscing elit. Vivamus mollis elit purus, a ornare lacus gravida vitae. Proin consectetur felis vel purus auctor, eu lacinia sapien scelerisque. Vivamus sit amet neque et quam tincidunt hendrerit. Praesent semper egestas tellus id dignissim. Pellentesque odio lectus, iaculis ac volutpat et, blandit quis urna. Sed vestibulum nisi sit amet nisl venenatis tempus. Donec molestie blandit quam, et porta nunc laoreet in. Integer sit amet scelerisque nisi.
        "},
        indoc! {"
            # Hello

            Lorem ipsum dolor sit amet, ˇconsectetur adipiscing elit. Vivamus mollis elit
            purus, a ornare lacus gravida vitae. Proin consectetur felis vel purus auctor,
            eu lacinia sapien scelerisque. Vivamus sit amet neque et quam tincidunt
            hendrerit. Praesent semper egestas tellus id dignissim. Pellentesque odio
            lectus, iaculis ac volutpat et, blandit quis urna. Sed vestibulum nisi sit amet
            nisl venenatis tempus. Donec molestie blandit quam, et porta nunc laoreet in.
            Integer sit amet scelerisque nisi.
        "},
        markdown_language,
        &mut cx,
    );

    assert_rewrap(
        indoc! {"
            Lorem ipsum dolor sit amet, ˇconsectetur adipiscing elit. Vivamus mollis elit purus, a ornare lacus gravida vitae. Proin consectetur felis vel purus auctor, eu lacinia sapien scelerisque. Vivamus sit amet neque et quam tincidunt hendrerit. Praesent semper egestas tellus id dignissim. Pellentesque odio lectus, iaculis ac volutpat et, blandit quis urna. Sed vestibulum nisi sit amet nisl venenatis tempus. Donec molestie blandit quam, et porta nunc laoreet in. Integer sit amet scelerisque nisi.
        "},
        indoc! {"
            Lorem ipsum dolor sit amet, ˇconsectetur adipiscing elit. Vivamus mollis elit
            purus, a ornare lacus gravida vitae. Proin consectetur felis vel purus auctor,
            eu lacinia sapien scelerisque. Vivamus sit amet neque et quam tincidunt
            hendrerit. Praesent semper egestas tellus id dignissim. Pellentesque odio
            lectus, iaculis ac volutpat et, blandit quis urna. Sed vestibulum nisi sit amet
            nisl venenatis tempus. Donec molestie blandit quam, et porta nunc laoreet in.
            Integer sit amet scelerisque nisi.
        "},
        plaintext_language,
        &mut cx,
    );

    // Test rewrapping unaligned comments in a selection.
    assert_rewrap(
        indoc! {"
            fn foo() {
                if true {
            «        // Lorem ipsum dolor sit amet, consectetur adipiscing elit. Vivamus mollis elit purus, a ornare lacus gravida vitae.
            // Praesent semper egestas tellus id dignissim.ˇ»
                    do_something();
                } else {
                    //
                }
            }
        "},
        indoc! {"
            fn foo() {
                if true {
            «        // Lorem ipsum dolor sit amet, consectetur adipiscing elit. Vivamus
                    // mollis elit purus, a ornare lacus gravida vitae. Praesent semper
                    // egestas tellus id dignissim.ˇ»
                    do_something();
                } else {
                    //
                }
            }
        "},
        language_with_doc_comments.clone(),
        &mut cx,
    );

    assert_rewrap(
        indoc! {"
            fn foo() {
                if true {
            «ˇ        // Lorem ipsum dolor sit amet, consectetur adipiscing elit. Vivamus mollis elit purus, a ornare lacus gravida vitae.
            // Praesent semper egestas tellus id dignissim.»
                    do_something();
                } else {
                    //
                }

            }
        "},
        indoc! {"
            fn foo() {
                if true {
            «ˇ        // Lorem ipsum dolor sit amet, consectetur adipiscing elit. Vivamus
                    // mollis elit purus, a ornare lacus gravida vitae. Praesent semper
                    // egestas tellus id dignissim.»
                    do_something();
                } else {
                    //
                }

            }
        "},
        language_with_doc_comments.clone(),
        &mut cx,
    );

    #[track_caller]
    fn assert_rewrap(
        unwrapped_text: &str,
        wrapped_text: &str,
        language: Arc<Language>,
        cx: &mut EditorTestContext,
    ) {
        cx.update_buffer(|buffer, cx| buffer.set_language(Some(language), cx));
        cx.set_state(unwrapped_text);
        cx.update_editor(|e, cx| e.rewrap(&Rewrap, cx));
        cx.assert_editor_state(wrapped_text);
    }
}

#[gpui::test]
async fn test_clipboard(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;

    cx.set_state("«one✅ ˇ»two «three ˇ»four «five ˇ»six ");
    cx.update_editor(|e, cx| e.cut(&Cut, cx));
    cx.assert_editor_state("ˇtwo ˇfour ˇsix ");

    // Paste with three cursors. Each cursor pastes one slice of the clipboard text.
    cx.set_state("two ˇfour ˇsix ˇ");
    cx.update_editor(|e, cx| e.paste(&Paste, cx));
    cx.assert_editor_state("two one✅ ˇfour three ˇsix five ˇ");

    // Paste again but with only two cursors. Since the number of cursors doesn't
    // match the number of slices in the clipboard, the entire clipboard text
    // is pasted at each cursor.
    cx.set_state("ˇtwo one✅ four three six five ˇ");
    cx.update_editor(|e, cx| {
        e.handle_input("( ", cx);
        e.paste(&Paste, cx);
        e.handle_input(") ", cx);
    });
    cx.assert_editor_state(
        &([
            "( one✅ ",
            "three ",
            "five ) ˇtwo one✅ four three six five ( one✅ ",
            "three ",
            "five ) ˇ",
        ]
        .join("\n")),
    );

    // Cut with three selections, one of which is full-line.
    cx.set_state(indoc! {"
        1«2ˇ»3
        4ˇ567
        «8ˇ»9"});
    cx.update_editor(|e, cx| e.cut(&Cut, cx));
    cx.assert_editor_state(indoc! {"
        1ˇ3
        ˇ9"});

    // Paste with three selections, noticing how the copied selection that was full-line
    // gets inserted before the second cursor.
    cx.set_state(indoc! {"
        1ˇ3
        9ˇ
        «oˇ»ne"});
    cx.update_editor(|e, cx| e.paste(&Paste, cx));
    cx.assert_editor_state(indoc! {"
        12ˇ3
        4567
        9ˇ
        8ˇne"});

    // Copy with a single cursor only, which writes the whole line into the clipboard.
    cx.set_state(indoc! {"
        The quick brown
        fox juˇmps over
        the lazy dog"});
    cx.update_editor(|e, cx| e.copy(&Copy, cx));
    assert_eq!(
        cx.read_from_clipboard()
            .and_then(|item| item.text().as_deref().map(str::to_string)),
        Some("fox jumps over\n".to_string())
    );

    // Paste with three selections, noticing how the copied full-line selection is inserted
    // before the empty selections but replaces the selection that is non-empty.
    cx.set_state(indoc! {"
        Tˇhe quick brown
        «foˇ»x jumps over
        tˇhe lazy dog"});
    cx.update_editor(|e, cx| e.paste(&Paste, cx));
    cx.assert_editor_state(indoc! {"
        fox jumps over
        Tˇhe quick brown
        fox jumps over
        ˇx jumps over
        fox jumps over
        tˇhe lazy dog"});
}

#[gpui::test]
async fn test_paste_multiline(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;
    let language = Arc::new(Language::new(
        LanguageConfig::default(),
        Some(tree_sitter_rust::LANGUAGE.into()),
    ));
    cx.update_buffer(|buffer, cx| buffer.set_language(Some(language), cx));

    // Cut an indented block, without the leading whitespace.
    cx.set_state(indoc! {"
        const a: B = (
            c(),
            «d(
                e,
                f
            )ˇ»
        );
    "});
    cx.update_editor(|e, cx| e.cut(&Cut, cx));
    cx.assert_editor_state(indoc! {"
        const a: B = (
            c(),
            ˇ
        );
    "});

    // Paste it at the same position.
    cx.update_editor(|e, cx| e.paste(&Paste, cx));
    cx.assert_editor_state(indoc! {"
        const a: B = (
            c(),
            d(
                e,
                f
            )ˇ
        );
    "});

    // Paste it at a line with a lower indent level.
    cx.set_state(indoc! {"
        ˇ
        const a: B = (
            c(),
        );
    "});
    cx.update_editor(|e, cx| e.paste(&Paste, cx));
    cx.assert_editor_state(indoc! {"
        d(
            e,
            f
        )ˇ
        const a: B = (
            c(),
        );
    "});

    // Cut an indented block, with the leading whitespace.
    cx.set_state(indoc! {"
        const a: B = (
            c(),
        «    d(
                e,
                f
            )
        ˇ»);
    "});
    cx.update_editor(|e, cx| e.cut(&Cut, cx));
    cx.assert_editor_state(indoc! {"
        const a: B = (
            c(),
        ˇ);
    "});

    // Paste it at the same position.
    cx.update_editor(|e, cx| e.paste(&Paste, cx));
    cx.assert_editor_state(indoc! {"
        const a: B = (
            c(),
            d(
                e,
                f
            )
        ˇ);
    "});

    // Paste it at a line with a higher indent level.
    cx.set_state(indoc! {"
        const a: B = (
            c(),
            d(
                e,
                fˇ
            )
        );
    "});
    cx.update_editor(|e, cx| e.paste(&Paste, cx));
    cx.assert_editor_state(indoc! {"
        const a: B = (
            c(),
            d(
                e,
                f    d(
                    e,
                    f
                )
        ˇ
            )
        );
    "});
}

#[gpui::test]
fn test_select_all(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let view = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple("abc\nde\nfgh", cx);
        build_editor(buffer, cx)
    });
    _ = view.update(cx, |view, cx| {
        view.select_all(&SelectAll, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            &[DisplayPoint::new(DisplayRow(0), 0)..DisplayPoint::new(DisplayRow(2), 3)]
        );
    });
}

#[gpui::test]
fn test_select_line(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let view = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple(&sample_text(6, 5, 'a'), cx);
        build_editor(buffer, cx)
    });
    _ = view.update(cx, |view, cx| {
        view.change_selections(None, cx, |s| {
            s.select_display_ranges([
                DisplayPoint::new(DisplayRow(0), 0)..DisplayPoint::new(DisplayRow(0), 1),
                DisplayPoint::new(DisplayRow(0), 2)..DisplayPoint::new(DisplayRow(0), 2),
                DisplayPoint::new(DisplayRow(1), 0)..DisplayPoint::new(DisplayRow(1), 0),
                DisplayPoint::new(DisplayRow(4), 2)..DisplayPoint::new(DisplayRow(4), 2),
            ])
        });
        view.select_line(&SelectLine, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            vec![
                DisplayPoint::new(DisplayRow(0), 0)..DisplayPoint::new(DisplayRow(2), 0),
                DisplayPoint::new(DisplayRow(4), 0)..DisplayPoint::new(DisplayRow(5), 0),
            ]
        );
    });

    _ = view.update(cx, |view, cx| {
        view.select_line(&SelectLine, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            vec![
                DisplayPoint::new(DisplayRow(0), 0)..DisplayPoint::new(DisplayRow(3), 0),
                DisplayPoint::new(DisplayRow(4), 0)..DisplayPoint::new(DisplayRow(5), 5),
            ]
        );
    });

    _ = view.update(cx, |view, cx| {
        view.select_line(&SelectLine, cx);
        assert_eq!(
            view.selections.display_ranges(cx),
            vec![DisplayPoint::new(DisplayRow(0), 0)..DisplayPoint::new(DisplayRow(5), 5)]
        );
    });
}

#[gpui::test]
fn test_split_selection_into_lines(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let view = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple(&sample_text(9, 5, 'a'), cx);
        build_editor(buffer, cx)
    });
    _ = view.update(cx, |view, cx| {
        view.fold_creases(
            vec![
                Crease::simple(Point::new(0, 2)..Point::new(1, 2), FoldPlaceholder::test()),
                Crease::simple(Point::new(2, 3)..Point::new(4, 1), FoldPlaceholder::test()),
                Crease::simple(Point::new(7, 0)..Point::new(8, 4), FoldPlaceholder::test()),
            ],
            true,
            cx,
        );
        view.change_selections(None, cx, |s| {
            s.select_display_ranges([
                DisplayPoint::new(DisplayRow(0), 0)..DisplayPoint::new(DisplayRow(0), 1),
                DisplayPoint::new(DisplayRow(0), 2)..DisplayPoint::new(DisplayRow(0), 2),
                DisplayPoint::new(DisplayRow(1), 0)..DisplayPoint::new(DisplayRow(1), 0),
                DisplayPoint::new(DisplayRow(4), 4)..DisplayPoint::new(DisplayRow(4), 4),
            ])
        });
        assert_eq!(view.display_text(cx), "aa⋯bbb\nccc⋯eeee\nfffff\nggggg\n⋯i");
    });

    _ = view.update(cx, |view, cx| {
        view.split_selection_into_lines(&SplitSelectionIntoLines, cx);
        assert_eq!(
            view.display_text(cx),
            "aaaaa\nbbbbb\nccc⋯eeee\nfffff\nggggg\n⋯i"
        );
        assert_eq!(
            view.selections.display_ranges(cx),
            [
                DisplayPoint::new(DisplayRow(0), 1)..DisplayPoint::new(DisplayRow(0), 1),
                DisplayPoint::new(DisplayRow(0), 2)..DisplayPoint::new(DisplayRow(0), 2),
                DisplayPoint::new(DisplayRow(2), 0)..DisplayPoint::new(DisplayRow(2), 0),
                DisplayPoint::new(DisplayRow(5), 4)..DisplayPoint::new(DisplayRow(5), 4)
            ]
        );
    });

    _ = view.update(cx, |view, cx| {
        view.change_selections(None, cx, |s| {
            s.select_display_ranges([
                DisplayPoint::new(DisplayRow(5), 0)..DisplayPoint::new(DisplayRow(0), 1)
            ])
        });
        view.split_selection_into_lines(&SplitSelectionIntoLines, cx);
        assert_eq!(
            view.display_text(cx),
            "aaaaa\nbbbbb\nccccc\nddddd\neeeee\nfffff\nggggg\nhhhhh\niiiii"
        );
        assert_eq!(
            view.selections.display_ranges(cx),
            [
                DisplayPoint::new(DisplayRow(0), 5)..DisplayPoint::new(DisplayRow(0), 5),
                DisplayPoint::new(DisplayRow(1), 5)..DisplayPoint::new(DisplayRow(1), 5),
                DisplayPoint::new(DisplayRow(2), 5)..DisplayPoint::new(DisplayRow(2), 5),
                DisplayPoint::new(DisplayRow(3), 5)..DisplayPoint::new(DisplayRow(3), 5),
                DisplayPoint::new(DisplayRow(4), 5)..DisplayPoint::new(DisplayRow(4), 5),
                DisplayPoint::new(DisplayRow(5), 5)..DisplayPoint::new(DisplayRow(5), 5),
                DisplayPoint::new(DisplayRow(6), 5)..DisplayPoint::new(DisplayRow(6), 5),
                DisplayPoint::new(DisplayRow(7), 0)..DisplayPoint::new(DisplayRow(7), 0)
            ]
        );
    });
}

#[gpui::test]
async fn test_add_selection_above_below(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;

    // let buffer = MultiBuffer::build_simple("abc\ndefghi\n\njk\nlmno\n", cx);
    cx.set_state(indoc!(
        r#"abc
           defˇghi

           jk
           nlmo
           "#
    ));

    cx.update_editor(|editor, cx| {
        editor.add_selection_above(&Default::default(), cx);
    });

    cx.assert_editor_state(indoc!(
        r#"abcˇ
           defˇghi

           jk
           nlmo
           "#
    ));

    cx.update_editor(|editor, cx| {
        editor.add_selection_above(&Default::default(), cx);
    });

    cx.assert_editor_state(indoc!(
        r#"abcˇ
            defˇghi

            jk
            nlmo
            "#
    ));

    cx.update_editor(|view, cx| {
        view.add_selection_below(&Default::default(), cx);
    });

    cx.assert_editor_state(indoc!(
        r#"abc
           defˇghi

           jk
           nlmo
           "#
    ));

    cx.update_editor(|view, cx| {
        view.undo_selection(&Default::default(), cx);
    });

    cx.assert_editor_state(indoc!(
        r#"abcˇ
           defˇghi

           jk
           nlmo
           "#
    ));

    cx.update_editor(|view, cx| {
        view.redo_selection(&Default::default(), cx);
    });

    cx.assert_editor_state(indoc!(
        r#"abc
           defˇghi

           jk
           nlmo
           "#
    ));

    cx.update_editor(|view, cx| {
        view.add_selection_below(&Default::default(), cx);
    });

    cx.assert_editor_state(indoc!(
        r#"abc
           defˇghi

           jk
           nlmˇo
           "#
    ));

    cx.update_editor(|view, cx| {
        view.add_selection_below(&Default::default(), cx);
    });

    cx.assert_editor_state(indoc!(
        r#"abc
           defˇghi

           jk
           nlmˇo
           "#
    ));

    // change selections
    cx.set_state(indoc!(
        r#"abc
           def«ˇg»hi

           jk
           nlmo
           "#
    ));

    cx.update_editor(|view, cx| {
        view.add_selection_below(&Default::default(), cx);
    });

    cx.assert_editor_state(indoc!(
        r#"abc
           def«ˇg»hi

           jk
           nlm«ˇo»
           "#
    ));

    cx.update_editor(|view, cx| {
        view.add_selection_below(&Default::default(), cx);
    });

    cx.assert_editor_state(indoc!(
        r#"abc
           def«ˇg»hi

           jk
           nlm«ˇo»
           "#
    ));

    cx.update_editor(|view, cx| {
        view.add_selection_above(&Default::default(), cx);
    });

    cx.assert_editor_state(indoc!(
        r#"abc
           def«ˇg»hi

           jk
           nlmo
           "#
    ));

    cx.update_editor(|view, cx| {
        view.add_selection_above(&Default::default(), cx);
    });

    cx.assert_editor_state(indoc!(
        r#"abc
           def«ˇg»hi

           jk
           nlmo
           "#
    ));

    // Change selections again
    cx.set_state(indoc!(
        r#"a«bc
           defgˇ»hi

           jk
           nlmo
           "#
    ));

    cx.update_editor(|view, cx| {
        view.add_selection_below(&Default::default(), cx);
    });

    cx.assert_editor_state(indoc!(
        r#"a«bcˇ»
           d«efgˇ»hi

           j«kˇ»
           nlmo
           "#
    ));

    cx.update_editor(|view, cx| {
        view.add_selection_below(&Default::default(), cx);
    });
    cx.assert_editor_state(indoc!(
        r#"a«bcˇ»
           d«efgˇ»hi

           j«kˇ»
           n«lmoˇ»
           "#
    ));
    cx.update_editor(|view, cx| {
        view.add_selection_above(&Default::default(), cx);
    });

    cx.assert_editor_state(indoc!(
        r#"a«bcˇ»
           d«efgˇ»hi

           j«kˇ»
           nlmo
           "#
    ));

    // Change selections again
    cx.set_state(indoc!(
        r#"abc
           d«ˇefghi

           jk
           nlm»o
           "#
    ));

    cx.update_editor(|view, cx| {
        view.add_selection_above(&Default::default(), cx);
    });

    cx.assert_editor_state(indoc!(
        r#"a«ˇbc»
           d«ˇef»ghi

           j«ˇk»
           n«ˇlm»o
           "#
    ));

    cx.update_editor(|view, cx| {
        view.add_selection_below(&Default::default(), cx);
    });

    cx.assert_editor_state(indoc!(
        r#"abc
           d«ˇef»ghi

           j«ˇk»
           n«ˇlm»o
           "#
    ));
}

#[gpui::test]
async fn test_select_next(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;
    cx.set_state("abc\nˇabc abc\ndefabc\nabc");

    cx.update_editor(|e, cx| e.select_next(&SelectNext::default(), cx))
        .unwrap();
    cx.assert_editor_state("abc\n«abcˇ» abc\ndefabc\nabc");

    cx.update_editor(|e, cx| e.select_next(&SelectNext::default(), cx))
        .unwrap();
    cx.assert_editor_state("abc\n«abcˇ» «abcˇ»\ndefabc\nabc");

    cx.update_editor(|view, cx| view.undo_selection(&UndoSelection, cx));
    cx.assert_editor_state("abc\n«abcˇ» abc\ndefabc\nabc");

    cx.update_editor(|view, cx| view.redo_selection(&RedoSelection, cx));
    cx.assert_editor_state("abc\n«abcˇ» «abcˇ»\ndefabc\nabc");

    cx.update_editor(|e, cx| e.select_next(&SelectNext::default(), cx))
        .unwrap();
    cx.assert_editor_state("abc\n«abcˇ» «abcˇ»\ndefabc\n«abcˇ»");

    cx.update_editor(|e, cx| e.select_next(&SelectNext::default(), cx))
        .unwrap();
    cx.assert_editor_state("«abcˇ»\n«abcˇ» «abcˇ»\ndefabc\n«abcˇ»");
}

#[gpui::test]
async fn test_select_all_matches(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;
    cx.set_state("abc\nˇabc abc\ndefabc\nabc");

    cx.update_editor(|e, cx| e.select_all_matches(&SelectAllMatches, cx))
        .unwrap();
    cx.assert_editor_state("«abcˇ»\n«abcˇ» «abcˇ»\ndefabc\n«abcˇ»");
}

#[gpui::test]
async fn test_select_next_with_multiple_carets(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;
    cx.set_state(
        r#"let foo = 2;
lˇet foo = 2;
let fooˇ = 2;
let foo = 2;
let foo = ˇ2;"#,
    );

    cx.update_editor(|e, cx| e.select_next(&SelectNext::default(), cx))
        .unwrap();
    cx.assert_editor_state(
        r#"let foo = 2;
«letˇ» foo = 2;
let «fooˇ» = 2;
let foo = 2;
let foo = «2ˇ»;"#,
    );

    // noop for multiple selections with different contents
    cx.update_editor(|e, cx| e.select_next(&SelectNext::default(), cx))
        .unwrap();
    cx.assert_editor_state(
        r#"let foo = 2;
«letˇ» foo = 2;
let «fooˇ» = 2;
let foo = 2;
let foo = «2ˇ»;"#,
    );
}

#[gpui::test]
async fn test_select_previous_multibuffer(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let mut cx =
        EditorTestContext::new_multibuffer(cx, ["aaa\n«bbb\nccc\n»ddd", "aaa\n«bbb\nccc\n»ddd"]);

    cx.assert_editor_state(indoc! {"
        ˇbbb
        ccc

        bbb
        ccc
        "});
    cx.dispatch_action(SelectPrevious::default());
    cx.assert_editor_state(indoc! {"
                «bbbˇ»
                ccc

                bbb
                ccc
                "});
    cx.dispatch_action(SelectPrevious::default());
    cx.assert_editor_state(indoc! {"
                «bbbˇ»
                ccc

                «bbbˇ»
                ccc
                "});
}

#[gpui::test]
async fn test_select_previous_with_single_caret(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;
    cx.set_state("abc\nˇabc abc\ndefabc\nabc");

    cx.update_editor(|e, cx| e.select_previous(&SelectPrevious::default(), cx))
        .unwrap();
    cx.assert_editor_state("abc\n«abcˇ» abc\ndefabc\nabc");

    cx.update_editor(|e, cx| e.select_previous(&SelectPrevious::default(), cx))
        .unwrap();
    cx.assert_editor_state("«abcˇ»\n«abcˇ» abc\ndefabc\nabc");

    cx.update_editor(|view, cx| view.undo_selection(&UndoSelection, cx));
    cx.assert_editor_state("abc\n«abcˇ» abc\ndefabc\nabc");

    cx.update_editor(|view, cx| view.redo_selection(&RedoSelection, cx));
    cx.assert_editor_state("«abcˇ»\n«abcˇ» abc\ndefabc\nabc");

    cx.update_editor(|e, cx| e.select_previous(&SelectPrevious::default(), cx))
        .unwrap();
    cx.assert_editor_state("«abcˇ»\n«abcˇ» abc\ndefabc\n«abcˇ»");

    cx.update_editor(|e, cx| e.select_previous(&SelectPrevious::default(), cx))
        .unwrap();
    cx.assert_editor_state("«abcˇ»\n«abcˇ» abc\ndef«abcˇ»\n«abcˇ»");

    cx.update_editor(|e, cx| e.select_previous(&SelectPrevious::default(), cx))
        .unwrap();
    cx.assert_editor_state("«abcˇ»\n«abcˇ» «abcˇ»\ndef«abcˇ»\n«abcˇ»");
}

#[gpui::test]
async fn test_select_previous_with_multiple_carets(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;
    cx.set_state(
        r#"let foo = 2;
lˇet foo = 2;
let fooˇ = 2;
let foo = 2;
let foo = ˇ2;"#,
    );

    cx.update_editor(|e, cx| e.select_previous(&SelectPrevious::default(), cx))
        .unwrap();
    cx.assert_editor_state(
        r#"let foo = 2;
«letˇ» foo = 2;
let «fooˇ» = 2;
let foo = 2;
let foo = «2ˇ»;"#,
    );

    // noop for multiple selections with different contents
    cx.update_editor(|e, cx| e.select_previous(&SelectPrevious::default(), cx))
        .unwrap();
    cx.assert_editor_state(
        r#"let foo = 2;
«letˇ» foo = 2;
let «fooˇ» = 2;
let foo = 2;
let foo = «2ˇ»;"#,
    );
}

#[gpui::test]
async fn test_select_previous_with_single_selection(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;
    cx.set_state("abc\n«ˇabc» abc\ndefabc\nabc");

    cx.update_editor(|e, cx| e.select_previous(&SelectPrevious::default(), cx))
        .unwrap();
    cx.assert_editor_state("«abcˇ»\n«ˇabc» abc\ndefabc\nabc");

    cx.update_editor(|e, cx| e.select_previous(&SelectPrevious::default(), cx))
        .unwrap();
    cx.assert_editor_state("«abcˇ»\n«ˇabc» abc\ndefabc\n«abcˇ»");

    cx.update_editor(|view, cx| view.undo_selection(&UndoSelection, cx));
    cx.assert_editor_state("«abcˇ»\n«ˇabc» abc\ndefabc\nabc");

    cx.update_editor(|view, cx| view.redo_selection(&RedoSelection, cx));
    cx.assert_editor_state("«abcˇ»\n«ˇabc» abc\ndefabc\n«abcˇ»");

    cx.update_editor(|e, cx| e.select_previous(&SelectPrevious::default(), cx))
        .unwrap();
    cx.assert_editor_state("«abcˇ»\n«ˇabc» abc\ndef«abcˇ»\n«abcˇ»");

    cx.update_editor(|e, cx| e.select_previous(&SelectPrevious::default(), cx))
        .unwrap();
    cx.assert_editor_state("«abcˇ»\n«ˇabc» «abcˇ»\ndef«abcˇ»\n«abcˇ»");
}

#[gpui::test]
async fn test_select_larger_smaller_syntax_node(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let language = Arc::new(Language::new(
        LanguageConfig::default(),
        Some(tree_sitter_rust::LANGUAGE.into()),
    ));

    let text = r#"
        use mod1::mod2::{mod3, mod4};

        fn fn_1(param1: bool, param2: &str) {
            let var1 = "text";
        }
    "#
    .unindent();

    let buffer = cx.new_model(|cx| Buffer::local(text, cx).with_language(language, cx));
    let buffer = cx.new_model(|cx| MultiBuffer::singleton(buffer, cx));
    let (editor, cx) = cx.add_window_view(|cx| build_editor(buffer, cx));

    editor
        .condition::<crate::EditorEvent>(cx, |view, cx| !view.buffer.read(cx).is_parsing(cx))
        .await;

    editor.update(cx, |view, cx| {
        view.change_selections(None, cx, |s| {
            s.select_display_ranges([
                DisplayPoint::new(DisplayRow(0), 25)..DisplayPoint::new(DisplayRow(0), 25),
                DisplayPoint::new(DisplayRow(2), 24)..DisplayPoint::new(DisplayRow(2), 12),
                DisplayPoint::new(DisplayRow(3), 18)..DisplayPoint::new(DisplayRow(3), 18),
            ]);
        });
        view.select_larger_syntax_node(&SelectLargerSyntaxNode, cx);
    });
    editor.update(cx, |editor, cx| {
        assert_text_with_selections(
            editor,
            indoc! {r#"
                use mod1::mod2::{mod3, «mod4ˇ»};

                fn fn_1«ˇ(param1: bool, param2: &str)» {
                    let var1 = "«textˇ»";
                }
            "#},
            cx,
        );
    });

    editor.update(cx, |view, cx| {
        view.select_larger_syntax_node(&SelectLargerSyntaxNode, cx);
    });
    editor.update(cx, |editor, cx| {
        assert_text_with_selections(
            editor,
            indoc! {r#"
                use mod1::mod2::«{mod3, mod4}ˇ»;

                «ˇfn fn_1(param1: bool, param2: &str) {
                    let var1 = "text";
                }»
            "#},
            cx,
        );
    });

    editor.update(cx, |view, cx| {
        view.select_larger_syntax_node(&SelectLargerSyntaxNode, cx);
    });
    assert_eq!(
        editor.update(cx, |view, cx| view.selections.display_ranges(cx)),
        &[DisplayPoint::new(DisplayRow(5), 0)..DisplayPoint::new(DisplayRow(0), 0)]
    );

    // Trying to expand the selected syntax node one more time has no effect.
    editor.update(cx, |view, cx| {
        view.select_larger_syntax_node(&SelectLargerSyntaxNode, cx);
    });
    assert_eq!(
        editor.update(cx, |view, cx| view.selections.display_ranges(cx)),
        &[DisplayPoint::new(DisplayRow(5), 0)..DisplayPoint::new(DisplayRow(0), 0)]
    );

    editor.update(cx, |view, cx| {
        view.select_smaller_syntax_node(&SelectSmallerSyntaxNode, cx);
    });
    editor.update(cx, |editor, cx| {
        assert_text_with_selections(
            editor,
            indoc! {r#"
                use mod1::mod2::«{mod3, mod4}ˇ»;

                «ˇfn fn_1(param1: bool, param2: &str) {
                    let var1 = "text";
                }»
            "#},
            cx,
        );
    });

    editor.update(cx, |view, cx| {
        view.select_smaller_syntax_node(&SelectSmallerSyntaxNode, cx);
    });
    editor.update(cx, |editor, cx| {
        assert_text_with_selections(
            editor,
            indoc! {r#"
                use mod1::mod2::{mod3, «mod4ˇ»};

                fn fn_1«ˇ(param1: bool, param2: &str)» {
                    let var1 = "«textˇ»";
                }
            "#},
            cx,
        );
    });

    editor.update(cx, |view, cx| {
        view.select_smaller_syntax_node(&SelectSmallerSyntaxNode, cx);
    });
    editor.update(cx, |editor, cx| {
        assert_text_with_selections(
            editor,
            indoc! {r#"
                use mod1::mod2::{mod3, mo«ˇ»d4};

                fn fn_1(para«ˇm1: bool, pa»ram2: &str) {
                    let var1 = "te«ˇ»xt";
                }
            "#},
            cx,
        );
    });

    // Trying to shrink the selected syntax node one more time has no effect.
    editor.update(cx, |view, cx| {
        view.select_smaller_syntax_node(&SelectSmallerSyntaxNode, cx);
    });
    editor.update(cx, |editor, cx| {
        assert_text_with_selections(
            editor,
            indoc! {r#"
                use mod1::mod2::{mod3, mo«ˇ»d4};

                fn fn_1(para«ˇm1: bool, pa»ram2: &str) {
                    let var1 = "te«ˇ»xt";
                }
            "#},
            cx,
        );
    });

    // Ensure that we keep expanding the selection if the larger selection starts or ends within
    // a fold.
    editor.update(cx, |view, cx| {
        view.fold_creases(
            vec![
                Crease::simple(
                    Point::new(0, 21)..Point::new(0, 24),
                    FoldPlaceholder::test(),
                ),
                Crease::simple(
                    Point::new(3, 20)..Point::new(3, 22),
                    FoldPlaceholder::test(),
                ),
            ],
            true,
            cx,
        );
        view.select_larger_syntax_node(&SelectLargerSyntaxNode, cx);
    });
    editor.update(cx, |editor, cx| {
        assert_text_with_selections(
            editor,
            indoc! {r#"
                use mod1::mod2::«{mod3, mod4}ˇ»;

                fn fn_1«ˇ(param1: bool, param2: &str)» {
                    «let var1 = "text";ˇ»
                }
            "#},
            cx,
        );
    });
}

#[gpui::test]
async fn test_autoindent(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let language = Arc::new(
        Language::new(
            LanguageConfig {
                brackets: BracketPairConfig {
                    pairs: vec![
                        BracketPair {
                            start: "{".to_string(),
                            end: "}".to_string(),
                            close: false,
                            surround: false,
                            newline: true,
                        },
                        BracketPair {
                            start: "(".to_string(),
                            end: ")".to_string(),
                            close: false,
                            surround: false,
                            newline: true,
                        },
                    ],
                    ..Default::default()
                },
                ..Default::default()
            },
            Some(tree_sitter_rust::LANGUAGE.into()),
        )
        .with_indents_query(
            r#"
                (_ "(" ")" @end) @indent
                (_ "{" "}" @end) @indent
            "#,
        )
        .unwrap(),
    );

    let text = "fn a() {}";

    let buffer = cx.new_model(|cx| Buffer::local(text, cx).with_language(language, cx));
    let buffer = cx.new_model(|cx| MultiBuffer::singleton(buffer, cx));
    let (editor, cx) = cx.add_window_view(|cx| build_editor(buffer, cx));
    editor
        .condition::<crate::EditorEvent>(cx, |editor, cx| !editor.buffer.read(cx).is_parsing(cx))
        .await;

    editor.update(cx, |editor, cx| {
        editor.change_selections(None, cx, |s| s.select_ranges([5..5, 8..8, 9..9]));
        editor.newline(&Newline, cx);
        assert_eq!(editor.text(cx), "fn a(\n    \n) {\n    \n}\n");
        assert_eq!(
            editor.selections.ranges(cx),
            &[
                Point::new(1, 4)..Point::new(1, 4),
                Point::new(3, 4)..Point::new(3, 4),
                Point::new(5, 0)..Point::new(5, 0)
            ]
        );
    });
}

#[gpui::test]
async fn test_autoindent_selections(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    {
        let mut cx = EditorLspTestContext::new_rust(Default::default(), cx).await;
        cx.set_state(indoc! {"
            impl A {

                fn b() {}

            «fn c() {

            }ˇ»
            }
        "});

        cx.update_editor(|editor, cx| {
            editor.autoindent(&Default::default(), cx);
        });

        cx.assert_editor_state(indoc! {"
            impl A {

                fn b() {}

                «fn c() {

                }ˇ»
            }
        "});
    }

    {
        let mut cx = EditorTestContext::new_multibuffer(
            cx,
            [indoc! { "
                impl A {
                «
                // a
                fn b(){}
                »
                «
                    }
                    fn c(){}
                »
            "}],
        );

        let buffer = cx.update_editor(|editor, cx| {
            let buffer = editor.buffer().update(cx, |buffer, _| {
                buffer.all_buffers().iter().next().unwrap().clone()
            });
            buffer.update(cx, |buffer, cx| buffer.set_language(Some(rust_lang()), cx));
            buffer
        });

        cx.run_until_parked();
        cx.update_editor(|editor, cx| {
            editor.select_all(&Default::default(), cx);
            editor.autoindent(&Default::default(), cx)
        });
        cx.run_until_parked();

        cx.update(|cx| {
            pretty_assertions::assert_eq!(
                buffer.read(cx).text(),
                indoc! { "
                    impl A {

                        // a
                        fn b(){}


                    }
                    fn c(){}

                " }
            )
        });
    }
}

#[gpui::test]
async fn test_autoclose_and_auto_surround_pairs(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;

    let language = Arc::new(Language::new(
        LanguageConfig {
            brackets: BracketPairConfig {
                pairs: vec![
                    BracketPair {
                        start: "{".to_string(),
                        end: "}".to_string(),
                        close: true,
                        surround: true,
                        newline: true,
                    },
                    BracketPair {
                        start: "(".to_string(),
                        end: ")".to_string(),
                        close: true,
                        surround: true,
                        newline: true,
                    },
                    BracketPair {
                        start: "/*".to_string(),
                        end: " */".to_string(),
                        close: true,
                        surround: true,
                        newline: true,
                    },
                    BracketPair {
                        start: "[".to_string(),
                        end: "]".to_string(),
                        close: false,
                        surround: false,
                        newline: true,
                    },
                    BracketPair {
                        start: "\"".to_string(),
                        end: "\"".to_string(),
                        close: true,
                        surround: true,
                        newline: false,
                    },
                    BracketPair {
                        start: "<".to_string(),
                        end: ">".to_string(),
                        close: false,
                        surround: true,
                        newline: true,
                    },
                ],
                ..Default::default()
            },
            autoclose_before: "})]".to_string(),
            ..Default::default()
        },
        Some(tree_sitter_rust::LANGUAGE.into()),
    ));

    cx.language_registry().add(language.clone());
    cx.update_buffer(|buffer, cx| {
        buffer.set_language(Some(language), cx);
    });

    cx.set_state(
        &r#"
            🏀ˇ
            εˇ
            ❤️ˇ
        "#
        .unindent(),
    );

    // autoclose multiple nested brackets at multiple cursors
    cx.update_editor(|view, cx| {
        view.handle_input("{", cx);
        view.handle_input("{", cx);
        view.handle_input("{", cx);
    });
    cx.assert_editor_state(
        &"
            🏀{{{ˇ}}}
            ε{{{ˇ}}}
            ❤️{{{ˇ}}}
        "
        .unindent(),
    );

    // insert a different closing bracket
    cx.update_editor(|view, cx| {
        view.handle_input(")", cx);
    });
    cx.assert_editor_state(
        &"
            🏀{{{)ˇ}}}
            ε{{{)ˇ}}}
            ❤️{{{)ˇ}}}
        "
        .unindent(),
    );

    // skip over the auto-closed brackets when typing a closing bracket
    cx.update_editor(|view, cx| {
        view.move_right(&MoveRight, cx);
        view.handle_input("}", cx);
        view.handle_input("}", cx);
        view.handle_input("}", cx);
    });
    cx.assert_editor_state(
        &"
            🏀{{{)}}}}ˇ
            ε{{{)}}}}ˇ
            ❤️{{{)}}}}ˇ
        "
        .unindent(),
    );

    // autoclose multi-character pairs
    cx.set_state(
        &"
            ˇ
            ˇ
        "
        .unindent(),
    );
    cx.update_editor(|view, cx| {
        view.handle_input("/", cx);
        view.handle_input("*", cx);
    });
    cx.assert_editor_state(
        &"
            /*ˇ */
            /*ˇ */
        "
        .unindent(),
    );

    // one cursor autocloses a multi-character pair, one cursor
    // does not autoclose.
    cx.set_state(
        &"
            /ˇ
            ˇ
        "
        .unindent(),
    );
    cx.update_editor(|view, cx| view.handle_input("*", cx));
    cx.assert_editor_state(
        &"
            /*ˇ */
            *ˇ
        "
        .unindent(),
    );

    // Don't autoclose if the next character isn't whitespace and isn't
    // listed in the language's "autoclose_before" section.
    cx.set_state("ˇa b");
    cx.update_editor(|view, cx| view.handle_input("{", cx));
    cx.assert_editor_state("{ˇa b");

    // Don't autoclose if `close` is false for the bracket pair
    cx.set_state("ˇ");
    cx.update_editor(|view, cx| view.handle_input("[", cx));
    cx.assert_editor_state("[ˇ");

    // Surround with brackets if text is selected
    cx.set_state("«aˇ» b");
    cx.update_editor(|view, cx| view.handle_input("{", cx));
    cx.assert_editor_state("{«aˇ»} b");

    // Autclose pair where the start and end characters are the same
    cx.set_state("aˇ");
    cx.update_editor(|view, cx| view.handle_input("\"", cx));
    cx.assert_editor_state("a\"ˇ\"");
    cx.update_editor(|view, cx| view.handle_input("\"", cx));
    cx.assert_editor_state("a\"\"ˇ");

    // Don't autoclose pair if autoclose is disabled
    cx.set_state("ˇ");
    cx.update_editor(|view, cx| view.handle_input("<", cx));
    cx.assert_editor_state("<ˇ");

    // Surround with brackets if text is selected and auto_surround is enabled, even if autoclose is disabled
    cx.set_state("«aˇ» b");
    cx.update_editor(|view, cx| view.handle_input("<", cx));
    cx.assert_editor_state("<«aˇ»> b");
}

#[gpui::test]
async fn test_always_treat_brackets_as_autoclosed_skip_over(cx: &mut gpui::TestAppContext) {
    init_test(cx, |settings| {
        settings.defaults.always_treat_brackets_as_autoclosed = Some(true);
    });

    let mut cx = EditorTestContext::new(cx).await;

    let language = Arc::new(Language::new(
        LanguageConfig {
            brackets: BracketPairConfig {
                pairs: vec![
                    BracketPair {
                        start: "{".to_string(),
                        end: "}".to_string(),
                        close: true,
                        surround: true,
                        newline: true,
                    },
                    BracketPair {
                        start: "(".to_string(),
                        end: ")".to_string(),
                        close: true,
                        surround: true,
                        newline: true,
                    },
                    BracketPair {
                        start: "[".to_string(),
                        end: "]".to_string(),
                        close: false,
                        surround: false,
                        newline: true,
                    },
                ],
                ..Default::default()
            },
            autoclose_before: "})]".to_string(),
            ..Default::default()
        },
        Some(tree_sitter_rust::LANGUAGE.into()),
    ));

    cx.language_registry().add(language.clone());
    cx.update_buffer(|buffer, cx| {
        buffer.set_language(Some(language), cx);
    });

    cx.set_state(
        &"
            ˇ
            ˇ
            ˇ
        "
        .unindent(),
    );

    // ensure only matching closing brackets are skipped over
    cx.update_editor(|view, cx| {
        view.handle_input("}", cx);
        view.move_left(&MoveLeft, cx);
        view.handle_input(")", cx);
        view.move_left(&MoveLeft, cx);
    });
    cx.assert_editor_state(
        &"
            ˇ)}
            ˇ)}
            ˇ)}
        "
        .unindent(),
    );

    // skip-over closing brackets at multiple cursors
    cx.update_editor(|view, cx| {
        view.handle_input(")", cx);
        view.handle_input("}", cx);
    });
    cx.assert_editor_state(
        &"
            )}ˇ
            )}ˇ
            )}ˇ
        "
        .unindent(),
    );

    // ignore non-close brackets
    cx.update_editor(|view, cx| {
        view.handle_input("]", cx);
        view.move_left(&MoveLeft, cx);
        view.handle_input("]", cx);
    });
    cx.assert_editor_state(
        &"
            )}]ˇ]
            )}]ˇ]
            )}]ˇ]
        "
        .unindent(),
    );
}

#[gpui::test]
async fn test_autoclose_with_embedded_language(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;

    let html_language = Arc::new(
        Language::new(
            LanguageConfig {
                name: "HTML".into(),
                brackets: BracketPairConfig {
                    pairs: vec![
                        BracketPair {
                            start: "<".into(),
                            end: ">".into(),
                            close: true,
                            ..Default::default()
                        },
                        BracketPair {
                            start: "{".into(),
                            end: "}".into(),
                            close: true,
                            ..Default::default()
                        },
                        BracketPair {
                            start: "(".into(),
                            end: ")".into(),
                            close: true,
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
                autoclose_before: "})]>".into(),
                ..Default::default()
            },
            Some(tree_sitter_html::language()),
        )
        .with_injection_query(
            r#"
            (script_element
                (raw_text) @content
                (#set! "language" "javascript"))
            "#,
        )
        .unwrap(),
    );

    let javascript_language = Arc::new(Language::new(
        LanguageConfig {
            name: "JavaScript".into(),
            brackets: BracketPairConfig {
                pairs: vec![
                    BracketPair {
                        start: "/*".into(),
                        end: " */".into(),
                        close: true,
                        ..Default::default()
                    },
                    BracketPair {
                        start: "{".into(),
                        end: "}".into(),
                        close: true,
                        ..Default::default()
                    },
                    BracketPair {
                        start: "(".into(),
                        end: ")".into(),
                        close: true,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            autoclose_before: "})]>".into(),
            ..Default::default()
        },
        Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
    ));

    cx.language_registry().add(html_language.clone());
    cx.language_registry().add(javascript_language.clone());

    cx.update_buffer(|buffer, cx| {
        buffer.set_language(Some(html_language), cx);
    });

    cx.set_state(
        &r#"
            <body>ˇ
                <script>
                    var x = 1;ˇ
                </script>
            </body>ˇ
        "#
        .unindent(),
    );

    // Precondition: different languages are active at different locations.
    cx.update_editor(|editor, cx| {
        let snapshot = editor.snapshot(cx);
        let cursors = editor.selections.ranges::<usize>(cx);
        let languages = cursors
            .iter()
            .map(|c| snapshot.language_at(c.start).unwrap().name())
            .collect::<Vec<_>>();
        assert_eq!(
            languages,
            &["HTML".into(), "JavaScript".into(), "HTML".into()]
        );
    });

    // Angle brackets autoclose in HTML, but not JavaScript.
    cx.update_editor(|editor, cx| {
        editor.handle_input("<", cx);
        editor.handle_input("a", cx);
    });
    cx.assert_editor_state(
        &r#"
            <body><aˇ>
                <script>
                    var x = 1;<aˇ
                </script>
            </body><aˇ>
        "#
        .unindent(),
    );

    // Curly braces and parens autoclose in both HTML and JavaScript.
    cx.update_editor(|editor, cx| {
        editor.handle_input(" b=", cx);
        editor.handle_input("{", cx);
        editor.handle_input("c", cx);
        editor.handle_input("(", cx);
    });
    cx.assert_editor_state(
        &r#"
            <body><a b={c(ˇ)}>
                <script>
                    var x = 1;<a b={c(ˇ)}
                </script>
            </body><a b={c(ˇ)}>
        "#
        .unindent(),
    );

    // Brackets that were already autoclosed are skipped.
    cx.update_editor(|editor, cx| {
        editor.handle_input(")", cx);
        editor.handle_input("d", cx);
        editor.handle_input("}", cx);
    });
    cx.assert_editor_state(
        &r#"
            <body><a b={c()d}ˇ>
                <script>
                    var x = 1;<a b={c()d}ˇ
                </script>
            </body><a b={c()d}ˇ>
        "#
        .unindent(),
    );
    cx.update_editor(|editor, cx| {
        editor.handle_input(">", cx);
    });
    cx.assert_editor_state(
        &r#"
            <body><a b={c()d}>ˇ
                <script>
                    var x = 1;<a b={c()d}>ˇ
                </script>
            </body><a b={c()d}>ˇ
        "#
        .unindent(),
    );

    // Reset
    cx.set_state(
        &r#"
            <body>ˇ
                <script>
                    var x = 1;ˇ
                </script>
            </body>ˇ
        "#
        .unindent(),
    );

    cx.update_editor(|editor, cx| {
        editor.handle_input("<", cx);
    });
    cx.assert_editor_state(
        &r#"
            <body><ˇ>
                <script>
                    var x = 1;<ˇ
                </script>
            </body><ˇ>
        "#
        .unindent(),
    );

    // When backspacing, the closing angle brackets are removed.
    cx.update_editor(|editor, cx| {
        editor.backspace(&Backspace, cx);
    });
    cx.assert_editor_state(
        &r#"
            <body>ˇ
                <script>
                    var x = 1;ˇ
                </script>
            </body>ˇ
        "#
        .unindent(),
    );

    // Block comments autoclose in JavaScript, but not HTML.
    cx.update_editor(|editor, cx| {
        editor.handle_input("/", cx);
        editor.handle_input("*", cx);
    });
    cx.assert_editor_state(
        &r#"
            <body>/*ˇ
                <script>
                    var x = 1;/*ˇ */
                </script>
            </body>/*ˇ
        "#
        .unindent(),
    );
}

#[gpui::test]
async fn test_autoclose_with_overrides(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;

    let rust_language = Arc::new(
        Language::new(
            LanguageConfig {
                name: "Rust".into(),
                brackets: serde_json::from_value(json!([
                    { "start": "{", "end": "}", "close": true, "newline": true },
                    { "start": "\"", "end": "\"", "close": true, "newline": false, "not_in": ["string"] },
                ]))
                .unwrap(),
                autoclose_before: "})]>".into(),
                ..Default::default()
            },
            Some(tree_sitter_rust::LANGUAGE.into()),
        )
        .with_override_query("(string_literal) @string")
        .unwrap(),
    );

    cx.language_registry().add(rust_language.clone());
    cx.update_buffer(|buffer, cx| {
        buffer.set_language(Some(rust_language), cx);
    });

    cx.set_state(
        &r#"
            let x = ˇ
        "#
        .unindent(),
    );

    // Inserting a quotation mark. A closing quotation mark is automatically inserted.
    cx.update_editor(|editor, cx| {
        editor.handle_input("\"", cx);
    });
    cx.assert_editor_state(
        &r#"
            let x = "ˇ"
        "#
        .unindent(),
    );

    // Inserting another quotation mark. The cursor moves across the existing
    // automatically-inserted quotation mark.
    cx.update_editor(|editor, cx| {
        editor.handle_input("\"", cx);
    });
    cx.assert_editor_state(
        &r#"
            let x = ""ˇ
        "#
        .unindent(),
    );

    // Reset
    cx.set_state(
        &r#"
            let x = ˇ
        "#
        .unindent(),
    );

    // Inserting a quotation mark inside of a string. A second quotation mark is not inserted.
    cx.update_editor(|editor, cx| {
        editor.handle_input("\"", cx);
        editor.handle_input(" ", cx);
        editor.move_left(&Default::default(), cx);
        editor.handle_input("\\", cx);
        editor.handle_input("\"", cx);
    });
    cx.assert_editor_state(
        &r#"
            let x = "\"ˇ "
        "#
        .unindent(),
    );

    // Inserting a closing quotation mark at the position of an automatically-inserted quotation
    // mark. Nothing is inserted.
    cx.update_editor(|editor, cx| {
        editor.move_right(&Default::default(), cx);
        editor.handle_input("\"", cx);
    });
    cx.assert_editor_state(
        &r#"
            let x = "\" "ˇ
        "#
        .unindent(),
    );
}

#[gpui::test]
async fn test_surround_with_pair(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let language = Arc::new(Language::new(
        LanguageConfig {
            brackets: BracketPairConfig {
                pairs: vec![
                    BracketPair {
                        start: "{".to_string(),
                        end: "}".to_string(),
                        close: true,
                        surround: true,
                        newline: true,
                    },
                    BracketPair {
                        start: "/* ".to_string(),
                        end: "*/".to_string(),
                        close: true,
                        surround: true,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            ..Default::default()
        },
        Some(tree_sitter_rust::LANGUAGE.into()),
    ));

    let text = r#"
        a
        b
        c
    "#
    .unindent();

    let buffer = cx.new_model(|cx| Buffer::local(text, cx).with_language(language, cx));
    let buffer = cx.new_model(|cx| MultiBuffer::singleton(buffer, cx));
    let (view, cx) = cx.add_window_view(|cx| build_editor(buffer, cx));
    view.condition::<crate::EditorEvent>(cx, |view, cx| !view.buffer.read(cx).is_parsing(cx))
        .await;

    view.update(cx, |view, cx| {
        view.change_selections(None, cx, |s| {
            s.select_display_ranges([
                DisplayPoint::new(DisplayRow(0), 0)..DisplayPoint::new(DisplayRow(0), 1),
                DisplayPoint::new(DisplayRow(1), 0)..DisplayPoint::new(DisplayRow(1), 1),
                DisplayPoint::new(DisplayRow(2), 0)..DisplayPoint::new(DisplayRow(2), 1),
            ])
        });

        view.handle_input("{", cx);
        view.handle_input("{", cx);
        view.handle_input("{", cx);
        assert_eq!(
            view.text(cx),
            "
                {{{a}}}
                {{{b}}}
                {{{c}}}
            "
            .unindent()
        );
        assert_eq!(
            view.selections.display_ranges(cx),
            [
                DisplayPoint::new(DisplayRow(0), 3)..DisplayPoint::new(DisplayRow(0), 4),
                DisplayPoint::new(DisplayRow(1), 3)..DisplayPoint::new(DisplayRow(1), 4),
                DisplayPoint::new(DisplayRow(2), 3)..DisplayPoint::new(DisplayRow(2), 4)
            ]
        );

        view.undo(&Undo, cx);
        view.undo(&Undo, cx);
        view.undo(&Undo, cx);
        assert_eq!(
            view.text(cx),
            "
                a
                b
                c
            "
            .unindent()
        );
        assert_eq!(
            view.selections.display_ranges(cx),
            [
                DisplayPoint::new(DisplayRow(0), 0)..DisplayPoint::new(DisplayRow(0), 1),
                DisplayPoint::new(DisplayRow(1), 0)..DisplayPoint::new(DisplayRow(1), 1),
                DisplayPoint::new(DisplayRow(2), 0)..DisplayPoint::new(DisplayRow(2), 1)
            ]
        );

        // Ensure inserting the first character of a multi-byte bracket pair
        // doesn't surround the selections with the bracket.
        view.handle_input("/", cx);
        assert_eq!(
            view.text(cx),
            "
                /
                /
                /
            "
            .unindent()
        );
        assert_eq!(
            view.selections.display_ranges(cx),
            [
                DisplayPoint::new(DisplayRow(0), 1)..DisplayPoint::new(DisplayRow(0), 1),
                DisplayPoint::new(DisplayRow(1), 1)..DisplayPoint::new(DisplayRow(1), 1),
                DisplayPoint::new(DisplayRow(2), 1)..DisplayPoint::new(DisplayRow(2), 1)
            ]
        );

        view.undo(&Undo, cx);
        assert_eq!(
            view.text(cx),
            "
                a
                b
                c
            "
            .unindent()
        );
        assert_eq!(
            view.selections.display_ranges(cx),
            [
                DisplayPoint::new(DisplayRow(0), 0)..DisplayPoint::new(DisplayRow(0), 1),
                DisplayPoint::new(DisplayRow(1), 0)..DisplayPoint::new(DisplayRow(1), 1),
                DisplayPoint::new(DisplayRow(2), 0)..DisplayPoint::new(DisplayRow(2), 1)
            ]
        );

        // Ensure inserting the last character of a multi-byte bracket pair
        // doesn't surround the selections with the bracket.
        view.handle_input("*", cx);
        assert_eq!(
            view.text(cx),
            "
                *
                *
                *
            "
            .unindent()
        );
        assert_eq!(
            view.selections.display_ranges(cx),
            [
                DisplayPoint::new(DisplayRow(0), 1)..DisplayPoint::new(DisplayRow(0), 1),
                DisplayPoint::new(DisplayRow(1), 1)..DisplayPoint::new(DisplayRow(1), 1),
                DisplayPoint::new(DisplayRow(2), 1)..DisplayPoint::new(DisplayRow(2), 1)
            ]
        );
    });
}

#[gpui::test]
async fn test_delete_autoclose_pair(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let language = Arc::new(Language::new(
        LanguageConfig {
            brackets: BracketPairConfig {
                pairs: vec![BracketPair {
                    start: "{".to_string(),
                    end: "}".to_string(),
                    close: true,
                    surround: true,
                    newline: true,
                }],
                ..Default::default()
            },
            autoclose_before: "}".to_string(),
            ..Default::default()
        },
        Some(tree_sitter_rust::LANGUAGE.into()),
    ));

    let text = r#"
        a
        b
        c
    "#
    .unindent();

    let buffer = cx.new_model(|cx| Buffer::local(text, cx).with_language(language, cx));
    let buffer = cx.new_model(|cx| MultiBuffer::singleton(buffer, cx));
    let (editor, cx) = cx.add_window_view(|cx| build_editor(buffer, cx));
    editor
        .condition::<crate::EditorEvent>(cx, |view, cx| !view.buffer.read(cx).is_parsing(cx))
        .await;

    editor.update(cx, |editor, cx| {
        editor.change_selections(None, cx, |s| {
            s.select_ranges([
                Point::new(0, 1)..Point::new(0, 1),
                Point::new(1, 1)..Point::new(1, 1),
                Point::new(2, 1)..Point::new(2, 1),
            ])
        });

        editor.handle_input("{", cx);
        editor.handle_input("{", cx);
        editor.handle_input("_", cx);
        assert_eq!(
            editor.text(cx),
            "
                a{{_}}
                b{{_}}
                c{{_}}
            "
            .unindent()
        );
        assert_eq!(
            editor.selections.ranges::<Point>(cx),
            [
                Point::new(0, 4)..Point::new(0, 4),
                Point::new(1, 4)..Point::new(1, 4),
                Point::new(2, 4)..Point::new(2, 4)
            ]
        );

        editor.backspace(&Default::default(), cx);
        editor.backspace(&Default::default(), cx);
        assert_eq!(
            editor.text(cx),
            "
                a{}
                b{}
                c{}
            "
            .unindent()
        );
        assert_eq!(
            editor.selections.ranges::<Point>(cx),
            [
                Point::new(0, 2)..Point::new(0, 2),
                Point::new(1, 2)..Point::new(1, 2),
                Point::new(2, 2)..Point::new(2, 2)
            ]
        );

        editor.delete_to_previous_word_start(&Default::default(), cx);
        assert_eq!(
            editor.text(cx),
            "
                a
                b
                c
            "
            .unindent()
        );
        assert_eq!(
            editor.selections.ranges::<Point>(cx),
            [
                Point::new(0, 1)..Point::new(0, 1),
                Point::new(1, 1)..Point::new(1, 1),
                Point::new(2, 1)..Point::new(2, 1)
            ]
        );
    });
}

#[gpui::test]
async fn test_always_treat_brackets_as_autoclosed_delete(cx: &mut gpui::TestAppContext) {
    init_test(cx, |settings| {
        settings.defaults.always_treat_brackets_as_autoclosed = Some(true);
    });

    let mut cx = EditorTestContext::new(cx).await;

    let language = Arc::new(Language::new(
        LanguageConfig {
            brackets: BracketPairConfig {
                pairs: vec![
                    BracketPair {
                        start: "{".to_string(),
                        end: "}".to_string(),
                        close: true,
                        surround: true,
                        newline: true,
                    },
                    BracketPair {
                        start: "(".to_string(),
                        end: ")".to_string(),
                        close: true,
                        surround: true,
                        newline: true,
                    },
                    BracketPair {
                        start: "[".to_string(),
                        end: "]".to_string(),
                        close: false,
                        surround: true,
                        newline: true,
                    },
                ],
                ..Default::default()
            },
            autoclose_before: "})]".to_string(),
            ..Default::default()
        },
        Some(tree_sitter_rust::LANGUAGE.into()),
    ));

    cx.language_registry().add(language.clone());
    cx.update_buffer(|buffer, cx| {
        buffer.set_language(Some(language), cx);
    });

    cx.set_state(
        &"
            {(ˇ)}
            [[ˇ]]
            {(ˇ)}
        "
        .unindent(),
    );

    cx.update_editor(|view, cx| {
        view.backspace(&Default::default(), cx);
        view.backspace(&Default::default(), cx);
    });

    cx.assert_editor_state(
        &"
            ˇ
            ˇ]]
            ˇ
        "
        .unindent(),
    );

    cx.update_editor(|view, cx| {
        view.handle_input("{", cx);
        view.handle_input("{", cx);
        view.move_right(&MoveRight, cx);
        view.move_right(&MoveRight, cx);
        view.move_left(&MoveLeft, cx);
        view.move_left(&MoveLeft, cx);
        view.backspace(&Default::default(), cx);
    });

    cx.assert_editor_state(
        &"
            {ˇ}
            {ˇ}]]
            {ˇ}
        "
        .unindent(),
    );

    cx.update_editor(|view, cx| {
        view.backspace(&Default::default(), cx);
    });

    cx.assert_editor_state(
        &"
            ˇ
            ˇ]]
            ˇ
        "
        .unindent(),
    );
}

#[gpui::test]
async fn test_auto_replace_emoji_shortcode(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let language = Arc::new(Language::new(
        LanguageConfig::default(),
        Some(tree_sitter_rust::LANGUAGE.into()),
    ));

    let buffer = cx.new_model(|cx| Buffer::local("", cx).with_language(language, cx));
    let buffer = cx.new_model(|cx| MultiBuffer::singleton(buffer, cx));
    let (editor, cx) = cx.add_window_view(|cx| build_editor(buffer, cx));
    editor
        .condition::<crate::EditorEvent>(cx, |view, cx| !view.buffer.read(cx).is_parsing(cx))
        .await;

    editor.update(cx, |editor, cx| {
        editor.set_auto_replace_emoji_shortcode(true);

        editor.handle_input("Hello ", cx);
        editor.handle_input(":wave", cx);
        assert_eq!(editor.text(cx), "Hello :wave".unindent());

        editor.handle_input(":", cx);
        assert_eq!(editor.text(cx), "Hello 👋".unindent());

        editor.handle_input(" :smile", cx);
        assert_eq!(editor.text(cx), "Hello 👋 :smile".unindent());

        editor.handle_input(":", cx);
        assert_eq!(editor.text(cx), "Hello 👋 😄".unindent());

        // Ensure shortcode gets replaced when it is part of a word that only consists of emojis
        editor.handle_input(":wave", cx);
        assert_eq!(editor.text(cx), "Hello 👋 😄:wave".unindent());

        editor.handle_input(":", cx);
        assert_eq!(editor.text(cx), "Hello 👋 😄👋".unindent());

        editor.handle_input(":1", cx);
        assert_eq!(editor.text(cx), "Hello 👋 😄👋:1".unindent());

        editor.handle_input(":", cx);
        assert_eq!(editor.text(cx), "Hello 👋 😄👋:1:".unindent());

        // Ensure shortcode does not get replaced when it is part of a word
        editor.handle_input(" Test:wave", cx);
        assert_eq!(editor.text(cx), "Hello 👋 😄👋:1: Test:wave".unindent());

        editor.handle_input(":", cx);
        assert_eq!(editor.text(cx), "Hello 👋 😄👋:1: Test:wave:".unindent());

        editor.set_auto_replace_emoji_shortcode(false);

        // Ensure shortcode does not get replaced when auto replace is off
        editor.handle_input(" :wave", cx);
        assert_eq!(
            editor.text(cx),
            "Hello 👋 😄👋:1: Test:wave: :wave".unindent()
        );

        editor.handle_input(":", cx);
        assert_eq!(
            editor.text(cx),
            "Hello 👋 😄👋:1: Test:wave: :wave:".unindent()
        );
    });
}

#[gpui::test]
async fn test_snippet_placeholder_choices(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let (text, insertion_ranges) = marked_text_ranges(
        indoc! {"
            ˇ
        "},
        false,
    );

    let buffer = cx.update(|cx| MultiBuffer::build_simple(&text, cx));
    let (editor, cx) = cx.add_window_view(|cx| build_editor(buffer, cx));

    _ = editor.update(cx, |editor, cx| {
        let snippet = Snippet::parse("type ${1|,i32,u32|} = $2").unwrap();

        editor
            .insert_snippet(&insertion_ranges, snippet, cx)
            .unwrap();

        fn assert(editor: &mut Editor, cx: &mut ViewContext<Editor>, marked_text: &str) {
            let (expected_text, selection_ranges) = marked_text_ranges(marked_text, false);
            assert_eq!(editor.text(cx), expected_text);
            assert_eq!(editor.selections.ranges::<usize>(cx), selection_ranges);
        }

        assert(
            editor,
            cx,
            indoc! {"
            type «» =•
            "},
        );

        assert!(editor.context_menu_visible(), "There should be a matches");
    });
}

#[gpui::test]
async fn test_snippets(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let (text, insertion_ranges) = marked_text_ranges(
        indoc! {"
            a.ˇ b
            a.ˇ b
            a.ˇ b
        "},
        false,
    );

    let buffer = cx.update(|cx| MultiBuffer::build_simple(&text, cx));
    let (editor, cx) = cx.add_window_view(|cx| build_editor(buffer, cx));

    editor.update(cx, |editor, cx| {
        let snippet = Snippet::parse("f(${1:one}, ${2:two}, ${1:three})$0").unwrap();

        editor
            .insert_snippet(&insertion_ranges, snippet, cx)
            .unwrap();

        fn assert(editor: &mut Editor, cx: &mut ViewContext<Editor>, marked_text: &str) {
            let (expected_text, selection_ranges) = marked_text_ranges(marked_text, false);
            assert_eq!(editor.text(cx), expected_text);
            assert_eq!(editor.selections.ranges::<usize>(cx), selection_ranges);
        }

        assert(
            editor,
            cx,
            indoc! {"
                a.f(«one», two, «three») b
                a.f(«one», two, «three») b
                a.f(«one», two, «three») b
            "},
        );

        // Can't move earlier than the first tab stop
        assert!(!editor.move_to_prev_snippet_tabstop(cx));
        assert(
            editor,
            cx,
            indoc! {"
                a.f(«one», two, «three») b
                a.f(«one», two, «three») b
                a.f(«one», two, «three») b
            "},
        );

        assert!(editor.move_to_next_snippet_tabstop(cx));
        assert(
            editor,
            cx,
            indoc! {"
                a.f(one, «two», three) b
                a.f(one, «two», three) b
                a.f(one, «two», three) b
            "},
        );

        editor.move_to_prev_snippet_tabstop(cx);
        assert(
            editor,
            cx,
            indoc! {"
                a.f(«one», two, «three») b
                a.f(«one», two, «three») b
                a.f(«one», two, «three») b
            "},
        );

        assert!(editor.move_to_next_snippet_tabstop(cx));
        assert(
            editor,
            cx,
            indoc! {"
                a.f(one, «two», three) b
                a.f(one, «two», three) b
                a.f(one, «two», three) b
            "},
        );
        assert!(editor.move_to_next_snippet_tabstop(cx));
        assert(
            editor,
            cx,
            indoc! {"
                a.f(one, two, three)ˇ b
                a.f(one, two, three)ˇ b
                a.f(one, two, three)ˇ b
            "},
        );

        // As soon as the last tab stop is reached, snippet state is gone
        editor.move_to_prev_snippet_tabstop(cx);
        assert(
            editor,
            cx,
            indoc! {"
                a.f(one, two, three)ˇ b
                a.f(one, two, three)ˇ b
                a.f(one, two, three)ˇ b
            "},
        );
    });
}

#[gpui::test]
async fn test_document_format_during_save(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let fs = FakeFs::new(cx.executor());
    fs.insert_file("/file.rs", Default::default()).await;

    let project = Project::test(fs, ["/file.rs".as_ref()], cx).await;

    let language_registry = project.read_with(cx, |project, _| project.languages().clone());
    language_registry.add(rust_lang());
    let mut fake_servers = language_registry.register_fake_lsp(
        "Rust",
        FakeLspAdapter {
            capabilities: lsp::ServerCapabilities {
                document_formatting_provider: Some(lsp::OneOf::Left(true)),
                ..Default::default()
            },
            ..Default::default()
        },
    );

    let buffer = project
        .update(cx, |project, cx| project.open_local_buffer("/file.rs", cx))
        .await
        .unwrap();

    let buffer = cx.new_model(|cx| MultiBuffer::singleton(buffer, cx));
    let (editor, cx) =
        cx.add_window_view(|cx| build_editor_with_project(project.clone(), buffer, cx));
    editor.update(cx, |editor, cx| editor.set_text("one\ntwo\nthree\n", cx));
    assert!(cx.read(|cx| editor.is_dirty(cx)));

    cx.executor().start_waiting();
    let fake_server = fake_servers.next().await.unwrap();

    let save = editor
        .update(cx, |editor, cx| editor.save(true, project.clone(), cx))
        .unwrap();
    fake_server
        .handle_request::<lsp::request::Formatting, _, _>(move |params, _| async move {
            assert_eq!(
                params.text_document.uri,
                lsp::Url::from_file_path("/file.rs").unwrap()
            );
            assert_eq!(params.options.tab_size, 4);
            Ok(Some(vec![lsp::TextEdit::new(
                lsp::Range::new(lsp::Position::new(0, 3), lsp::Position::new(1, 0)),
                ", ".to_string(),
            )]))
        })
        .next()
        .await;
    cx.executor().start_waiting();
    save.await;

    assert_eq!(
        editor.update(cx, |editor, cx| editor.text(cx)),
        "one, two\nthree\n"
    );
    assert!(!cx.read(|cx| editor.is_dirty(cx)));

    editor.update(cx, |editor, cx| editor.set_text("one\ntwo\nthree\n", cx));
    assert!(cx.read(|cx| editor.is_dirty(cx)));

    // Ensure we can still save even if formatting hangs.
    fake_server.handle_request::<lsp::request::Formatting, _, _>(move |params, _| async move {
        assert_eq!(
            params.text_document.uri,
            lsp::Url::from_file_path("/file.rs").unwrap()
        );
        futures::future::pending::<()>().await;
        unreachable!()
    });
    let save = editor
        .update(cx, |editor, cx| editor.save(true, project.clone(), cx))
        .unwrap();
    cx.executor().advance_clock(super::FORMAT_TIMEOUT);
    cx.executor().start_waiting();
    save.await;
    assert_eq!(
        editor.update(cx, |editor, cx| editor.text(cx)),
        "one\ntwo\nthree\n"
    );
    assert!(!cx.read(|cx| editor.is_dirty(cx)));

    // For non-dirty buffer, no formatting request should be sent
    let save = editor
        .update(cx, |editor, cx| editor.save(true, project.clone(), cx))
        .unwrap();
    let _pending_format_request = fake_server
        .handle_request::<lsp::request::RangeFormatting, _, _>(move |_, _| async move {
            panic!("Should not be invoked on non-dirty buffer");
        })
        .next();
    cx.executor().start_waiting();
    save.await;

    // Set rust language override and assert overridden tabsize is sent to language server
    update_test_language_settings(cx, |settings| {
        settings.languages.insert(
            "Rust".into(),
            LanguageSettingsContent {
                tab_size: NonZeroU32::new(8),
                ..Default::default()
            },
        );
    });

    editor.update(cx, |editor, cx| editor.set_text("somehting_new\n", cx));
    assert!(cx.read(|cx| editor.is_dirty(cx)));
    let save = editor
        .update(cx, |editor, cx| editor.save(true, project.clone(), cx))
        .unwrap();
    fake_server
        .handle_request::<lsp::request::Formatting, _, _>(move |params, _| async move {
            assert_eq!(
                params.text_document.uri,
                lsp::Url::from_file_path("/file.rs").unwrap()
            );
            assert_eq!(params.options.tab_size, 8);
            Ok(Some(vec![]))
        })
        .next()
        .await;
    cx.executor().start_waiting();
    save.await;
}

#[gpui::test]
async fn test_multibuffer_format_during_save(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let cols = 4;
    let rows = 10;
    let sample_text_1 = sample_text(rows, cols, 'a');
    assert_eq!(
        sample_text_1,
        "aaaa\nbbbb\ncccc\ndddd\neeee\nffff\ngggg\nhhhh\niiii\njjjj"
    );
    let sample_text_2 = sample_text(rows, cols, 'l');
    assert_eq!(
        sample_text_2,
        "llll\nmmmm\nnnnn\noooo\npppp\nqqqq\nrrrr\nssss\ntttt\nuuuu"
    );
    let sample_text_3 = sample_text(rows, cols, 'v');
    assert_eq!(
        sample_text_3,
        "vvvv\nwwww\nxxxx\nyyyy\nzzzz\n{{{{\n||||\n}}}}\n~~~~\n\u{7f}\u{7f}\u{7f}\u{7f}"
    );

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(
        "/a",
        json!({
            "main.rs": sample_text_1,
            "other.rs": sample_text_2,
            "lib.rs": sample_text_3,
        }),
    )
    .await;

    let project = Project::test(fs, ["/a".as_ref()], cx).await;
    let workspace = cx.add_window(|cx| Workspace::test_new(project.clone(), cx));
    let cx = &mut VisualTestContext::from_window(*workspace.deref(), cx);

    let language_registry = project.read_with(cx, |project, _| project.languages().clone());
    language_registry.add(rust_lang());
    let mut fake_servers = language_registry.register_fake_lsp(
        "Rust",
        FakeLspAdapter {
            capabilities: lsp::ServerCapabilities {
                document_formatting_provider: Some(lsp::OneOf::Left(true)),
                ..Default::default()
            },
            ..Default::default()
        },
    );

    let worktree = project.update(cx, |project, cx| {
        let mut worktrees = project.worktrees(cx).collect::<Vec<_>>();
        assert_eq!(worktrees.len(), 1);
        worktrees.pop().unwrap()
    });
    let worktree_id = worktree.update(cx, |worktree, _| worktree.id());

    let buffer_1 = project
        .update(cx, |project, cx| {
            project.open_buffer((worktree_id, "main.rs"), cx)
        })
        .await
        .unwrap();
    let buffer_2 = project
        .update(cx, |project, cx| {
            project.open_buffer((worktree_id, "other.rs"), cx)
        })
        .await
        .unwrap();
    let buffer_3 = project
        .update(cx, |project, cx| {
            project.open_buffer((worktree_id, "lib.rs"), cx)
        })
        .await
        .unwrap();

    let multi_buffer = cx.new_model(|cx| {
        let mut multi_buffer = MultiBuffer::new(ReadWrite);
        multi_buffer.push_excerpts(
            buffer_1.clone(),
            [
                ExcerptRange {
                    context: Point::new(0, 0)..Point::new(3, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(5, 0)..Point::new(7, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(9, 0)..Point::new(10, 4),
                    primary: None,
                },
            ],
            cx,
        );
        multi_buffer.push_excerpts(
            buffer_2.clone(),
            [
                ExcerptRange {
                    context: Point::new(0, 0)..Point::new(3, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(5, 0)..Point::new(7, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(9, 0)..Point::new(10, 4),
                    primary: None,
                },
            ],
            cx,
        );
        multi_buffer.push_excerpts(
            buffer_3.clone(),
            [
                ExcerptRange {
                    context: Point::new(0, 0)..Point::new(3, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(5, 0)..Point::new(7, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(9, 0)..Point::new(10, 4),
                    primary: None,
                },
            ],
            cx,
        );
        multi_buffer
    });
    let multi_buffer_editor = cx.new_view(|cx| {
        Editor::new(
            EditorMode::Full,
            multi_buffer,
            Some(project.clone()),
            true,
            cx,
        )
    });

    multi_buffer_editor.update(cx, |editor, cx| {
        editor.change_selections(Some(Autoscroll::Next), cx, |s| s.select_ranges(Some(1..2)));
        editor.insert("|one|two|three|", cx);
    });
    assert!(cx.read(|cx| multi_buffer_editor.is_dirty(cx)));
    multi_buffer_editor.update(cx, |editor, cx| {
        editor.change_selections(Some(Autoscroll::Next), cx, |s| {
            s.select_ranges(Some(60..70))
        });
        editor.insert("|four|five|six|", cx);
    });
    assert!(cx.read(|cx| multi_buffer_editor.is_dirty(cx)));

    // First two buffers should be edited, but not the third one.
    assert_eq!(
        multi_buffer_editor.update(cx, |editor, cx| editor.text(cx)),
        "a|one|two|three|aa\nbbbb\ncccc\n\nffff\ngggg\n\njjjj\nllll\nmmmm\nnnnn|four|five|six|\nr\n\nuuuu\nvvvv\nwwww\nxxxx\n\n{{{{\n||||\n\n\u{7f}\u{7f}\u{7f}\u{7f}",
    );
    buffer_1.update(cx, |buffer, _| {
        assert!(buffer.is_dirty());
        assert_eq!(
            buffer.text(),
            "a|one|two|three|aa\nbbbb\ncccc\ndddd\neeee\nffff\ngggg\nhhhh\niiii\njjjj",
        )
    });
    buffer_2.update(cx, |buffer, _| {
        assert!(buffer.is_dirty());
        assert_eq!(
            buffer.text(),
            "llll\nmmmm\nnnnn|four|five|six|oooo\npppp\nr\nssss\ntttt\nuuuu",
        )
    });
    buffer_3.update(cx, |buffer, _| {
        assert!(!buffer.is_dirty());
        assert_eq!(buffer.text(), sample_text_3,)
    });
    cx.executor().run_until_parked();

    cx.executor().start_waiting();
    let save = multi_buffer_editor
        .update(cx, |editor, cx| editor.save(true, project.clone(), cx))
        .unwrap();

    let fake_server = fake_servers.next().await.unwrap();
    fake_server
        .server
        .on_request::<lsp::request::Formatting, _, _>(move |params, _| async move {
            Ok(Some(vec![lsp::TextEdit::new(
                lsp::Range::new(lsp::Position::new(0, 3), lsp::Position::new(1, 0)),
                format!("[{} formatted]", params.text_document.uri),
            )]))
        })
        .detach();
    save.await;

    // After multibuffer saving, only first two buffers should be reformatted, but not the third one (as it was not dirty).
    assert!(cx.read(|cx| !multi_buffer_editor.is_dirty(cx)));
    assert_eq!(
        multi_buffer_editor.update(cx, |editor, cx| editor.text(cx)),
        "a|o[file:///a/main.rs formatted]bbbb\ncccc\n\nffff\ngggg\n\njjjj\n\nlll[file:///a/other.rs formatted]mmmm\nnnnn|four|five|six|\nr\n\nuuuu\n\nvvvv\nwwww\nxxxx\n\n{{{{\n||||\n\n\u{7f}\u{7f}\u{7f}\u{7f}",
    );
    buffer_1.update(cx, |buffer, _| {
        assert!(!buffer.is_dirty());
        assert_eq!(
            buffer.text(),
            "a|o[file:///a/main.rs formatted]bbbb\ncccc\ndddd\neeee\nffff\ngggg\nhhhh\niiii\njjjj\n",
        )
    });
    buffer_2.update(cx, |buffer, _| {
        assert!(!buffer.is_dirty());
        assert_eq!(
            buffer.text(),
            "lll[file:///a/other.rs formatted]mmmm\nnnnn|four|five|six|oooo\npppp\nr\nssss\ntttt\nuuuu\n",
        )
    });
    buffer_3.update(cx, |buffer, _| {
        assert!(!buffer.is_dirty());
        assert_eq!(buffer.text(), sample_text_3,)
    });
}

#[gpui::test]
async fn test_range_format_during_save(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let fs = FakeFs::new(cx.executor());
    fs.insert_file("/file.rs", Default::default()).await;

    let project = Project::test(fs, ["/file.rs".as_ref()], cx).await;

    let language_registry = project.read_with(cx, |project, _| project.languages().clone());
    language_registry.add(rust_lang());
    let mut fake_servers = language_registry.register_fake_lsp(
        "Rust",
        FakeLspAdapter {
            capabilities: lsp::ServerCapabilities {
                document_range_formatting_provider: Some(lsp::OneOf::Left(true)),
                ..Default::default()
            },
            ..Default::default()
        },
    );

    let buffer = project
        .update(cx, |project, cx| project.open_local_buffer("/file.rs", cx))
        .await
        .unwrap();

    let buffer = cx.new_model(|cx| MultiBuffer::singleton(buffer, cx));
    let (editor, cx) =
        cx.add_window_view(|cx| build_editor_with_project(project.clone(), buffer, cx));
    editor.update(cx, |editor, cx| editor.set_text("one\ntwo\nthree\n", cx));
    assert!(cx.read(|cx| editor.is_dirty(cx)));

    cx.executor().start_waiting();
    let fake_server = fake_servers.next().await.unwrap();

    let save = editor
        .update(cx, |editor, cx| editor.save(true, project.clone(), cx))
        .unwrap();
    fake_server
        .handle_request::<lsp::request::RangeFormatting, _, _>(move |params, _| async move {
            assert_eq!(
                params.text_document.uri,
                lsp::Url::from_file_path("/file.rs").unwrap()
            );
            assert_eq!(params.options.tab_size, 4);
            Ok(Some(vec![lsp::TextEdit::new(
                lsp::Range::new(lsp::Position::new(0, 3), lsp::Position::new(1, 0)),
                ", ".to_string(),
            )]))
        })
        .next()
        .await;
    cx.executor().start_waiting();
    save.await;
    assert_eq!(
        editor.update(cx, |editor, cx| editor.text(cx)),
        "one, two\nthree\n"
    );
    assert!(!cx.read(|cx| editor.is_dirty(cx)));

    editor.update(cx, |editor, cx| editor.set_text("one\ntwo\nthree\n", cx));
    assert!(cx.read(|cx| editor.is_dirty(cx)));

    // Ensure we can still save even if formatting hangs.
    fake_server.handle_request::<lsp::request::RangeFormatting, _, _>(
        move |params, _| async move {
            assert_eq!(
                params.text_document.uri,
                lsp::Url::from_file_path("/file.rs").unwrap()
            );
            futures::future::pending::<()>().await;
            unreachable!()
        },
    );
    let save = editor
        .update(cx, |editor, cx| editor.save(true, project.clone(), cx))
        .unwrap();
    cx.executor().advance_clock(super::FORMAT_TIMEOUT);
    cx.executor().start_waiting();
    save.await;
    assert_eq!(
        editor.update(cx, |editor, cx| editor.text(cx)),
        "one\ntwo\nthree\n"
    );
    assert!(!cx.read(|cx| editor.is_dirty(cx)));

    // For non-dirty buffer, no formatting request should be sent
    let save = editor
        .update(cx, |editor, cx| editor.save(true, project.clone(), cx))
        .unwrap();
    let _pending_format_request = fake_server
        .handle_request::<lsp::request::RangeFormatting, _, _>(move |_, _| async move {
            panic!("Should not be invoked on non-dirty buffer");
        })
        .next();
    cx.executor().start_waiting();
    save.await;

    // Set Rust language override and assert overridden tabsize is sent to language server
    update_test_language_settings(cx, |settings| {
        settings.languages.insert(
            "Rust".into(),
            LanguageSettingsContent {
                tab_size: NonZeroU32::new(8),
                ..Default::default()
            },
        );
    });

    editor.update(cx, |editor, cx| editor.set_text("somehting_new\n", cx));
    assert!(cx.read(|cx| editor.is_dirty(cx)));
    let save = editor
        .update(cx, |editor, cx| editor.save(true, project.clone(), cx))
        .unwrap();
    fake_server
        .handle_request::<lsp::request::RangeFormatting, _, _>(move |params, _| async move {
            assert_eq!(
                params.text_document.uri,
                lsp::Url::from_file_path("/file.rs").unwrap()
            );
            assert_eq!(params.options.tab_size, 8);
            Ok(Some(vec![]))
        })
        .next()
        .await;
    cx.executor().start_waiting();
    save.await;
}

#[gpui::test]
async fn test_document_format_manual_trigger(cx: &mut gpui::TestAppContext) {
    init_test(cx, |settings| {
        settings.defaults.formatter = Some(language_settings::SelectedFormatter::List(
            FormatterList(vec![Formatter::LanguageServer { name: None }].into()),
        ))
    });

    let fs = FakeFs::new(cx.executor());
    fs.insert_file("/file.rs", Default::default()).await;

    let project = Project::test(fs, ["/file.rs".as_ref()], cx).await;

    let language_registry = project.read_with(cx, |project, _| project.languages().clone());
    language_registry.add(Arc::new(Language::new(
        LanguageConfig {
            name: "Rust".into(),
            matcher: LanguageMatcher {
                path_suffixes: vec!["rs".to_string()],
                ..Default::default()
            },
            ..LanguageConfig::default()
        },
        Some(tree_sitter_rust::LANGUAGE.into()),
    )));
    update_test_language_settings(cx, |settings| {
        // Enable Prettier formatting for the same buffer, and ensure
        // LSP is called instead of Prettier.
        settings.defaults.prettier = Some(PrettierSettings {
            allowed: true,
            ..PrettierSettings::default()
        });
    });
    let mut fake_servers = language_registry.register_fake_lsp(
        "Rust",
        FakeLspAdapter {
            capabilities: lsp::ServerCapabilities {
                document_formatting_provider: Some(lsp::OneOf::Left(true)),
                ..Default::default()
            },
            ..Default::default()
        },
    );

    let buffer = project
        .update(cx, |project, cx| project.open_local_buffer("/file.rs", cx))
        .await
        .unwrap();

    let buffer = cx.new_model(|cx| MultiBuffer::singleton(buffer, cx));
    let (editor, cx) =
        cx.add_window_view(|cx| build_editor_with_project(project.clone(), buffer, cx));
    editor.update(cx, |editor, cx| editor.set_text("one\ntwo\nthree\n", cx));

    cx.executor().start_waiting();
    let fake_server = fake_servers.next().await.unwrap();

    let format = editor
        .update(cx, |editor, cx| {
            editor.perform_format(
                project.clone(),
                FormatTrigger::Manual,
                FormatTarget::Buffer,
                cx,
            )
        })
        .unwrap();
    fake_server
        .handle_request::<lsp::request::Formatting, _, _>(move |params, _| async move {
            assert_eq!(
                params.text_document.uri,
                lsp::Url::from_file_path("/file.rs").unwrap()
            );
            assert_eq!(params.options.tab_size, 4);
            Ok(Some(vec![lsp::TextEdit::new(
                lsp::Range::new(lsp::Position::new(0, 3), lsp::Position::new(1, 0)),
                ", ".to_string(),
            )]))
        })
        .next()
        .await;
    cx.executor().start_waiting();
    format.await;
    assert_eq!(
        editor.update(cx, |editor, cx| editor.text(cx)),
        "one, two\nthree\n"
    );

    editor.update(cx, |editor, cx| editor.set_text("one\ntwo\nthree\n", cx));
    // Ensure we don't lock if formatting hangs.
    fake_server.handle_request::<lsp::request::Formatting, _, _>(move |params, _| async move {
        assert_eq!(
            params.text_document.uri,
            lsp::Url::from_file_path("/file.rs").unwrap()
        );
        futures::future::pending::<()>().await;
        unreachable!()
    });
    let format = editor
        .update(cx, |editor, cx| {
            editor.perform_format(project, FormatTrigger::Manual, FormatTarget::Buffer, cx)
        })
        .unwrap();
    cx.executor().advance_clock(super::FORMAT_TIMEOUT);
    cx.executor().start_waiting();
    format.await;
    assert_eq!(
        editor.update(cx, |editor, cx| editor.text(cx)),
        "one\ntwo\nthree\n"
    );
}

#[gpui::test]
async fn test_concurrent_format_requests(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorLspTestContext::new_rust(
        lsp::ServerCapabilities {
            document_formatting_provider: Some(lsp::OneOf::Left(true)),
            ..Default::default()
        },
        cx,
    )
    .await;

    cx.set_state(indoc! {"
        one.twoˇ
    "});

    // The format request takes a long time. When it completes, it inserts
    // a newline and an indent before the `.`
    cx.lsp
        .handle_request::<lsp::request::Formatting, _, _>(move |_, cx| {
            let executor = cx.background_executor().clone();
            async move {
                executor.timer(Duration::from_millis(100)).await;
                Ok(Some(vec![lsp::TextEdit {
                    range: lsp::Range::new(lsp::Position::new(0, 3), lsp::Position::new(0, 3)),
                    new_text: "\n    ".into(),
                }]))
            }
        });

    // Submit a format request.
    let format_1 = cx
        .update_editor(|editor, cx| editor.format(&Format, cx))
        .unwrap();
    cx.executor().run_until_parked();

    // Submit a second format request.
    let format_2 = cx
        .update_editor(|editor, cx| editor.format(&Format, cx))
        .unwrap();
    cx.executor().run_until_parked();

    // Wait for both format requests to complete
    cx.executor().advance_clock(Duration::from_millis(200));
    cx.executor().start_waiting();
    format_1.await.unwrap();
    cx.executor().start_waiting();
    format_2.await.unwrap();

    // The formatting edits only happens once.
    cx.assert_editor_state(indoc! {"
        one
            .twoˇ
    "});
}

#[gpui::test]
async fn test_strip_whitespace_and_format_via_lsp(cx: &mut gpui::TestAppContext) {
    init_test(cx, |settings| {
        settings.defaults.formatter = Some(language_settings::SelectedFormatter::Auto)
    });

    let mut cx = EditorLspTestContext::new_rust(
        lsp::ServerCapabilities {
            document_formatting_provider: Some(lsp::OneOf::Left(true)),
            ..Default::default()
        },
        cx,
    )
    .await;

    // Set up a buffer white some trailing whitespace and no trailing newline.
    cx.set_state(
        &[
            "one ",   //
            "twoˇ",   //
            "three ", //
            "four",   //
        ]
        .join("\n"),
    );

    // Submit a format request.
    let format = cx
        .update_editor(|editor, cx| editor.format(&Format, cx))
        .unwrap();

    // Record which buffer changes have been sent to the language server
    let buffer_changes = Arc::new(Mutex::new(Vec::new()));
    cx.lsp
        .handle_notification::<lsp::notification::DidChangeTextDocument, _>({
            let buffer_changes = buffer_changes.clone();
            move |params, _| {
                buffer_changes.lock().extend(
                    params
                        .content_changes
                        .into_iter()
                        .map(|e| (e.range.unwrap(), e.text)),
                );
            }
        });

    // Handle formatting requests to the language server.
    cx.lsp.handle_request::<lsp::request::Formatting, _, _>({
        let buffer_changes = buffer_changes.clone();
        move |_, _| {
            // When formatting is requested, trailing whitespace has already been stripped,
            // and the trailing newline has already been added.
            assert_eq!(
                &buffer_changes.lock()[1..],
                &[
                    (
                        lsp::Range::new(lsp::Position::new(0, 3), lsp::Position::new(0, 4)),
                        "".into()
                    ),
                    (
                        lsp::Range::new(lsp::Position::new(2, 5), lsp::Position::new(2, 6)),
                        "".into()
                    ),
                    (
                        lsp::Range::new(lsp::Position::new(3, 4), lsp::Position::new(3, 4)),
                        "\n".into()
                    ),
                ]
            );

            // Insert blank lines between each line of the buffer.
            async move {
                Ok(Some(vec![
                    lsp::TextEdit {
                        range: lsp::Range::new(lsp::Position::new(1, 0), lsp::Position::new(1, 0)),
                        new_text: "\n".into(),
                    },
                    lsp::TextEdit {
                        range: lsp::Range::new(lsp::Position::new(2, 0), lsp::Position::new(2, 0)),
                        new_text: "\n".into(),
                    },
                ]))
            }
        }
    });

    // After formatting the buffer, the trailing whitespace is stripped,
    // a newline is appended, and the edits provided by the language server
    // have been applied.
    format.await.unwrap();
    cx.assert_editor_state(
        &[
            "one",   //
            "",      //
            "twoˇ",  //
            "",      //
            "three", //
            "four",  //
            "",      //
        ]
        .join("\n"),
    );

    // Undoing the formatting undoes the trailing whitespace removal, the
    // trailing newline, and the LSP edits.
    cx.update_buffer(|buffer, cx| buffer.undo(cx));
    cx.assert_editor_state(
        &[
            "one ",   //
            "twoˇ",   //
            "three ", //
            "four",   //
        ]
        .join("\n"),
    );
}

#[gpui::test]
async fn test_handle_input_for_show_signature_help_auto_signature_help_true(
    cx: &mut gpui::TestAppContext,
) {
    init_test(cx, |_| {});

    cx.update(|cx| {
        cx.update_global::<SettingsStore, _>(|settings, cx| {
            settings.update_user_settings::<EditorSettings>(cx, |settings| {
                settings.auto_signature_help = Some(true);
            });
        });
    });

    let mut cx = EditorLspTestContext::new_rust(
        lsp::ServerCapabilities {
            signature_help_provider: Some(lsp::SignatureHelpOptions {
                ..Default::default()
            }),
            ..Default::default()
        },
        cx,
    )
    .await;

    let language = Language::new(
        LanguageConfig {
            name: "Rust".into(),
            brackets: BracketPairConfig {
                pairs: vec![
                    BracketPair {
                        start: "{".to_string(),
                        end: "}".to_string(),
                        close: true,
                        surround: true,
                        newline: true,
                    },
                    BracketPair {
                        start: "(".to_string(),
                        end: ")".to_string(),
                        close: true,
                        surround: true,
                        newline: true,
                    },
                    BracketPair {
                        start: "/*".to_string(),
                        end: " */".to_string(),
                        close: true,
                        surround: true,
                        newline: true,
                    },
                    BracketPair {
                        start: "[".to_string(),
                        end: "]".to_string(),
                        close: false,
                        surround: false,
                        newline: true,
                    },
                    BracketPair {
                        start: "\"".to_string(),
                        end: "\"".to_string(),
                        close: true,
                        surround: true,
                        newline: false,
                    },
                    BracketPair {
                        start: "<".to_string(),
                        end: ">".to_string(),
                        close: false,
                        surround: true,
                        newline: true,
                    },
                ],
                ..Default::default()
            },
            autoclose_before: "})]".to_string(),
            ..Default::default()
        },
        Some(tree_sitter_rust::LANGUAGE.into()),
    );
    let language = Arc::new(language);

    cx.language_registry().add(language.clone());
    cx.update_buffer(|buffer, cx| {
        buffer.set_language(Some(language), cx);
    });

    cx.set_state(
        &r#"
            fn main() {
                sampleˇ
            }
        "#
        .unindent(),
    );

    cx.update_editor(|view, cx| {
        view.handle_input("(", cx);
    });
    cx.assert_editor_state(
        &"
            fn main() {
                sample(ˇ)
            }
        "
        .unindent(),
    );

    let mocked_response = lsp::SignatureHelp {
        signatures: vec![lsp::SignatureInformation {
            label: "fn sample(param1: u8, param2: u8)".to_string(),
            documentation: None,
            parameters: Some(vec![
                lsp::ParameterInformation {
                    label: lsp::ParameterLabel::Simple("param1: u8".to_string()),
                    documentation: None,
                },
                lsp::ParameterInformation {
                    label: lsp::ParameterLabel::Simple("param2: u8".to_string()),
                    documentation: None,
                },
            ]),
            active_parameter: None,
        }],
        active_signature: Some(0),
        active_parameter: Some(0),
    };
    handle_signature_help_request(&mut cx, mocked_response).await;

    cx.condition(|editor, _| editor.signature_help_state.is_shown())
        .await;

    cx.editor(|editor, _| {
        let signature_help_state = editor.signature_help_state.popover().cloned();
        assert!(signature_help_state.is_some());
        let ParsedMarkdown {
            text, highlights, ..
        } = signature_help_state.unwrap().parsed_content;
        assert_eq!(text, "param1: u8, param2: u8");
        assert_eq!(highlights, vec![(0..10, SIGNATURE_HELP_HIGHLIGHT_CURRENT)]);
    });
}

#[gpui::test]
async fn test_handle_input_with_different_show_signature_settings(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    cx.update(|cx| {
        cx.update_global::<SettingsStore, _>(|settings, cx| {
            settings.update_user_settings::<EditorSettings>(cx, |settings| {
                settings.auto_signature_help = Some(false);
                settings.show_signature_help_after_edits = Some(false);
            });
        });
    });

    let mut cx = EditorLspTestContext::new_rust(
        lsp::ServerCapabilities {
            signature_help_provider: Some(lsp::SignatureHelpOptions {
                ..Default::default()
            }),
            ..Default::default()
        },
        cx,
    )
    .await;

    let language = Language::new(
        LanguageConfig {
            name: "Rust".into(),
            brackets: BracketPairConfig {
                pairs: vec![
                    BracketPair {
                        start: "{".to_string(),
                        end: "}".to_string(),
                        close: true,
                        surround: true,
                        newline: true,
                    },
                    BracketPair {
                        start: "(".to_string(),
                        end: ")".to_string(),
                        close: true,
                        surround: true,
                        newline: true,
                    },
                    BracketPair {
                        start: "/*".to_string(),
                        end: " */".to_string(),
                        close: true,
                        surround: true,
                        newline: true,
                    },
                    BracketPair {
                        start: "[".to_string(),
                        end: "]".to_string(),
                        close: false,
                        surround: false,
                        newline: true,
                    },
                    BracketPair {
                        start: "\"".to_string(),
                        end: "\"".to_string(),
                        close: true,
                        surround: true,
                        newline: false,
                    },
                    BracketPair {
                        start: "<".to_string(),
                        end: ">".to_string(),
                        close: false,
                        surround: true,
                        newline: true,
                    },
                ],
                ..Default::default()
            },
            autoclose_before: "})]".to_string(),
            ..Default::default()
        },
        Some(tree_sitter_rust::LANGUAGE.into()),
    );
    let language = Arc::new(language);

    cx.language_registry().add(language.clone());
    cx.update_buffer(|buffer, cx| {
        buffer.set_language(Some(language), cx);
    });

    // Ensure that signature_help is not called when no signature help is enabled.
    cx.set_state(
        &r#"
            fn main() {
                sampleˇ
            }
        "#
        .unindent(),
    );
    cx.update_editor(|view, cx| {
        view.handle_input("(", cx);
    });
    cx.assert_editor_state(
        &"
            fn main() {
                sample(ˇ)
            }
        "
        .unindent(),
    );
    cx.editor(|editor, _| {
        assert!(editor.signature_help_state.task().is_none());
    });

    let mocked_response = lsp::SignatureHelp {
        signatures: vec![lsp::SignatureInformation {
            label: "fn sample(param1: u8, param2: u8)".to_string(),
            documentation: None,
            parameters: Some(vec![
                lsp::ParameterInformation {
                    label: lsp::ParameterLabel::Simple("param1: u8".to_string()),
                    documentation: None,
                },
                lsp::ParameterInformation {
                    label: lsp::ParameterLabel::Simple("param2: u8".to_string()),
                    documentation: None,
                },
            ]),
            active_parameter: None,
        }],
        active_signature: Some(0),
        active_parameter: Some(0),
    };

    // Ensure that signature_help is called when enabled afte edits
    cx.update(|cx| {
        cx.update_global::<SettingsStore, _>(|settings, cx| {
            settings.update_user_settings::<EditorSettings>(cx, |settings| {
                settings.auto_signature_help = Some(false);
                settings.show_signature_help_after_edits = Some(true);
            });
        });
    });
    cx.set_state(
        &r#"
            fn main() {
                sampleˇ
            }
        "#
        .unindent(),
    );
    cx.update_editor(|view, cx| {
        view.handle_input("(", cx);
    });
    cx.assert_editor_state(
        &"
            fn main() {
                sample(ˇ)
            }
        "
        .unindent(),
    );
    handle_signature_help_request(&mut cx, mocked_response.clone()).await;
    cx.condition(|editor, _| editor.signature_help_state.is_shown())
        .await;
    cx.update_editor(|editor, _| {
        let signature_help_state = editor.signature_help_state.popover().cloned();
        assert!(signature_help_state.is_some());
        let ParsedMarkdown {
            text, highlights, ..
        } = signature_help_state.unwrap().parsed_content;
        assert_eq!(text, "param1: u8, param2: u8");
        assert_eq!(highlights, vec![(0..10, SIGNATURE_HELP_HIGHLIGHT_CURRENT)]);
        editor.signature_help_state = SignatureHelpState::default();
    });

    // Ensure that signature_help is called when auto signature help override is enabled
    cx.update(|cx| {
        cx.update_global::<SettingsStore, _>(|settings, cx| {
            settings.update_user_settings::<EditorSettings>(cx, |settings| {
                settings.auto_signature_help = Some(true);
                settings.show_signature_help_after_edits = Some(false);
            });
        });
    });
    cx.set_state(
        &r#"
            fn main() {
                sampleˇ
            }
        "#
        .unindent(),
    );
    cx.update_editor(|view, cx| {
        view.handle_input("(", cx);
    });
    cx.assert_editor_state(
        &"
            fn main() {
                sample(ˇ)
            }
        "
        .unindent(),
    );
    handle_signature_help_request(&mut cx, mocked_response).await;
    cx.condition(|editor, _| editor.signature_help_state.is_shown())
        .await;
    cx.editor(|editor, _| {
        let signature_help_state = editor.signature_help_state.popover().cloned();
        assert!(signature_help_state.is_some());
        let ParsedMarkdown {
            text, highlights, ..
        } = signature_help_state.unwrap().parsed_content;
        assert_eq!(text, "param1: u8, param2: u8");
        assert_eq!(highlights, vec![(0..10, SIGNATURE_HELP_HIGHLIGHT_CURRENT)]);
    });
}

#[gpui::test]
async fn test_signature_help(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});
    cx.update(|cx| {
        cx.update_global::<SettingsStore, _>(|settings, cx| {
            settings.update_user_settings::<EditorSettings>(cx, |settings| {
                settings.auto_signature_help = Some(true);
            });
        });
    });

    let mut cx = EditorLspTestContext::new_rust(
        lsp::ServerCapabilities {
            signature_help_provider: Some(lsp::SignatureHelpOptions {
                ..Default::default()
            }),
            ..Default::default()
        },
        cx,
    )
    .await;

    // A test that directly calls `show_signature_help`
    cx.update_editor(|editor, cx| {
        editor.show_signature_help(&ShowSignatureHelp, cx);
    });

    let mocked_response = lsp::SignatureHelp {
        signatures: vec![lsp::SignatureInformation {
            label: "fn sample(param1: u8, param2: u8)".to_string(),
            documentation: None,
            parameters: Some(vec![
                lsp::ParameterInformation {
                    label: lsp::ParameterLabel::Simple("param1: u8".to_string()),
                    documentation: None,
                },
                lsp::ParameterInformation {
                    label: lsp::ParameterLabel::Simple("param2: u8".to_string()),
                    documentation: None,
                },
            ]),
            active_parameter: None,
        }],
        active_signature: Some(0),
        active_parameter: Some(0),
    };
    handle_signature_help_request(&mut cx, mocked_response).await;

    cx.condition(|editor, _| editor.signature_help_state.is_shown())
        .await;

    cx.editor(|editor, _| {
        let signature_help_state = editor.signature_help_state.popover().cloned();
        assert!(signature_help_state.is_some());
        let ParsedMarkdown {
            text, highlights, ..
        } = signature_help_state.unwrap().parsed_content;
        assert_eq!(text, "param1: u8, param2: u8");
        assert_eq!(highlights, vec![(0..10, SIGNATURE_HELP_HIGHLIGHT_CURRENT)]);
    });

    // When exiting outside from inside the brackets, `signature_help` is closed.
    cx.set_state(indoc! {"
        fn main() {
            sample(ˇ);
        }

        fn sample(param1: u8, param2: u8) {}
    "});

    cx.update_editor(|editor, cx| {
        editor.change_selections(None, cx, |s| s.select_ranges([0..0]));
    });

    let mocked_response = lsp::SignatureHelp {
        signatures: Vec::new(),
        active_signature: None,
        active_parameter: None,
    };
    handle_signature_help_request(&mut cx, mocked_response).await;

    cx.condition(|editor, _| !editor.signature_help_state.is_shown())
        .await;

    cx.editor(|editor, _| {
        assert!(!editor.signature_help_state.is_shown());
    });

    // When entering inside the brackets from outside, `show_signature_help` is automatically called.
    cx.set_state(indoc! {"
        fn main() {
            sample(ˇ);
        }

        fn sample(param1: u8, param2: u8) {}
    "});

    let mocked_response = lsp::SignatureHelp {
        signatures: vec![lsp::SignatureInformation {
            label: "fn sample(param1: u8, param2: u8)".to_string(),
            documentation: None,
            parameters: Some(vec![
                lsp::ParameterInformation {
                    label: lsp::ParameterLabel::Simple("param1: u8".to_string()),
                    documentation: None,
                },
                lsp::ParameterInformation {
                    label: lsp::ParameterLabel::Simple("param2: u8".to_string()),
                    documentation: None,
                },
            ]),
            active_parameter: None,
        }],
        active_signature: Some(0),
        active_parameter: Some(0),
    };
    handle_signature_help_request(&mut cx, mocked_response.clone()).await;
    cx.condition(|editor, _| editor.signature_help_state.is_shown())
        .await;
    cx.editor(|editor, _| {
        assert!(editor.signature_help_state.is_shown());
    });

    // Restore the popover with more parameter input
    cx.set_state(indoc! {"
        fn main() {
            sample(param1, param2ˇ);
        }

        fn sample(param1: u8, param2: u8) {}
    "});

    let mocked_response = lsp::SignatureHelp {
        signatures: vec![lsp::SignatureInformation {
            label: "fn sample(param1: u8, param2: u8)".to_string(),
            documentation: None,
            parameters: Some(vec![
                lsp::ParameterInformation {
                    label: lsp::ParameterLabel::Simple("param1: u8".to_string()),
                    documentation: None,
                },
                lsp::ParameterInformation {
                    label: lsp::ParameterLabel::Simple("param2: u8".to_string()),
                    documentation: None,
                },
            ]),
            active_parameter: None,
        }],
        active_signature: Some(0),
        active_parameter: Some(1),
    };
    handle_signature_help_request(&mut cx, mocked_response.clone()).await;
    cx.condition(|editor, _| editor.signature_help_state.is_shown())
        .await;

    // When selecting a range, the popover is gone.
    // Avoid using `cx.set_state` to not actually edit the document, just change its selections.
    cx.update_editor(|editor, cx| {
        editor.change_selections(None, cx, |s| {
            s.select_ranges(Some(Point::new(1, 25)..Point::new(1, 19)));
        })
    });
    cx.assert_editor_state(indoc! {"
        fn main() {
            sample(param1, «ˇparam2»);
        }

        fn sample(param1: u8, param2: u8) {}
    "});
    cx.editor(|editor, _| {
        assert!(!editor.signature_help_state.is_shown());
    });

    // When unselecting again, the popover is back if within the brackets.
    cx.update_editor(|editor, cx| {
        editor.change_selections(None, cx, |s| {
            s.select_ranges(Some(Point::new(1, 19)..Point::new(1, 19)));
        })
    });
    cx.assert_editor_state(indoc! {"
        fn main() {
            sample(param1, ˇparam2);
        }

        fn sample(param1: u8, param2: u8) {}
    "});
    handle_signature_help_request(&mut cx, mocked_response).await;
    cx.condition(|editor, _| editor.signature_help_state.is_shown())
        .await;
    cx.editor(|editor, _| {
        assert!(editor.signature_help_state.is_shown());
    });

    // Test to confirm that SignatureHelp does not appear after deselecting multiple ranges when it was hidden by pressing Escape.
    cx.update_editor(|editor, cx| {
        editor.change_selections(None, cx, |s| {
            s.select_ranges(Some(Point::new(0, 0)..Point::new(0, 0)));
            s.select_ranges(Some(Point::new(1, 19)..Point::new(1, 19)));
        })
    });
    cx.assert_editor_state(indoc! {"
        fn main() {
            sample(param1, ˇparam2);
        }

        fn sample(param1: u8, param2: u8) {}
    "});

    let mocked_response = lsp::SignatureHelp {
        signatures: vec![lsp::SignatureInformation {
            label: "fn sample(param1: u8, param2: u8)".to_string(),
            documentation: None,
            parameters: Some(vec![
                lsp::ParameterInformation {
                    label: lsp::ParameterLabel::Simple("param1: u8".to_string()),
                    documentation: None,
                },
                lsp::ParameterInformation {
                    label: lsp::ParameterLabel::Simple("param2: u8".to_string()),
                    documentation: None,
                },
            ]),
            active_parameter: None,
        }],
        active_signature: Some(0),
        active_parameter: Some(1),
    };
    handle_signature_help_request(&mut cx, mocked_response.clone()).await;
    cx.condition(|editor, _| editor.signature_help_state.is_shown())
        .await;
    cx.update_editor(|editor, cx| {
        editor.hide_signature_help(cx, SignatureHelpHiddenBy::Escape);
    });
    cx.condition(|editor, _| !editor.signature_help_state.is_shown())
        .await;
    cx.update_editor(|editor, cx| {
        editor.change_selections(None, cx, |s| {
            s.select_ranges(Some(Point::new(1, 25)..Point::new(1, 19)));
        })
    });
    cx.assert_editor_state(indoc! {"
        fn main() {
            sample(param1, «ˇparam2»);
        }

        fn sample(param1: u8, param2: u8) {}
    "});
    cx.update_editor(|editor, cx| {
        editor.change_selections(None, cx, |s| {
            s.select_ranges(Some(Point::new(1, 19)..Point::new(1, 19)));
        })
    });
    cx.assert_editor_state(indoc! {"
        fn main() {
            sample(param1, ˇparam2);
        }

        fn sample(param1: u8, param2: u8) {}
    "});
    cx.condition(|editor, _| !editor.signature_help_state.is_shown()) // because hidden by escape
        .await;
}

#[gpui::test]
async fn test_completion(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorLspTestContext::new_rust(
        lsp::ServerCapabilities {
            completion_provider: Some(lsp::CompletionOptions {
                trigger_characters: Some(vec![".".to_string(), ":".to_string()]),
                resolve_provider: Some(true),
                ..Default::default()
            }),
            signature_help_provider: Some(lsp::SignatureHelpOptions::default()),
            ..Default::default()
        },
        cx,
    )
    .await;
    let counter = Arc::new(AtomicUsize::new(0));

    cx.set_state(indoc! {"
        oneˇ
        two
        three
    "});
    cx.simulate_keystroke(".");
    handle_completion_request(
        &mut cx,
        indoc! {"
            one.|<>
            two
            three
        "},
        vec!["first_completion", "second_completion"],
        counter.clone(),
    )
    .await;
    cx.condition(|editor, _| editor.context_menu_visible())
        .await;
    assert_eq!(counter.load(atomic::Ordering::Acquire), 1);

    let _handler = handle_signature_help_request(
        &mut cx,
        lsp::SignatureHelp {
            signatures: vec![lsp::SignatureInformation {
                label: "test signature".to_string(),
                documentation: None,
                parameters: Some(vec![lsp::ParameterInformation {
                    label: lsp::ParameterLabel::Simple("foo: u8".to_string()),
                    documentation: None,
                }]),
                active_parameter: None,
            }],
            active_signature: None,
            active_parameter: None,
        },
    );
    cx.update_editor(|editor, cx| {
        assert!(
            !editor.signature_help_state.is_shown(),
            "No signature help was called for"
        );
        editor.show_signature_help(&ShowSignatureHelp, cx);
    });
    cx.run_until_parked();
    cx.update_editor(|editor, _| {
        assert!(
            !editor.signature_help_state.is_shown(),
            "No signature help should be shown when completions menu is open"
        );
    });

    let apply_additional_edits = cx.update_editor(|editor, cx| {
        editor.context_menu_next(&Default::default(), cx);
        editor
            .confirm_completion(&ConfirmCompletion::default(), cx)
            .unwrap()
    });
    cx.assert_editor_state(indoc! {"
        one.second_completionˇ
        two
        three
    "});

    handle_resolve_completion_request(
        &mut cx,
        Some(vec![
            (
                //This overlaps with the primary completion edit which is
                //misbehavior from the LSP spec, test that we filter it out
                indoc! {"
                    one.second_ˇcompletion
                    two
                    threeˇ
                "},
                "overlapping additional edit",
            ),
            (
                indoc! {"
                    one.second_completion
                    two
                    threeˇ
                "},
                "\nadditional edit",
            ),
        ]),
    )
    .await;
    apply_additional_edits.await.unwrap();
    cx.assert_editor_state(indoc! {"
        one.second_completionˇ
        two
        three
        additional edit
    "});

    cx.set_state(indoc! {"
        one.second_completion
        twoˇ
        threeˇ
        additional edit
    "});
    cx.simulate_keystroke(" ");
    assert!(cx.editor(|e, _| e.context_menu.borrow_mut().is_none()));
    cx.simulate_keystroke("s");
    assert!(cx.editor(|e, _| e.context_menu.borrow_mut().is_none()));

    cx.assert_editor_state(indoc! {"
        one.second_completion
        two sˇ
        three sˇ
        additional edit
    "});
    handle_completion_request(
        &mut cx,
        indoc! {"
            one.second_completion
            two s
            three <s|>
            additional edit
        "},
        vec!["fourth_completion", "fifth_completion", "sixth_completion"],
        counter.clone(),
    )
    .await;
    cx.condition(|editor, _| editor.context_menu_visible())
        .await;
    assert_eq!(counter.load(atomic::Ordering::Acquire), 2);

    cx.simulate_keystroke("i");

    handle_completion_request(
        &mut cx,
        indoc! {"
            one.second_completion
            two si
            three <si|>
            additional edit
        "},
        vec!["fourth_completion", "fifth_completion", "sixth_completion"],
        counter.clone(),
    )
    .await;
    cx.condition(|editor, _| editor.context_menu_visible())
        .await;
    assert_eq!(counter.load(atomic::Ordering::Acquire), 3);

    let apply_additional_edits = cx.update_editor(|editor, cx| {
        editor
            .confirm_completion(&ConfirmCompletion::default(), cx)
            .unwrap()
    });
    cx.assert_editor_state(indoc! {"
        one.second_completion
        two sixth_completionˇ
        three sixth_completionˇ
        additional edit
    "});

    apply_additional_edits.await.unwrap();

    update_test_language_settings(&mut cx, |settings| {
        settings.defaults.show_completions_on_input = Some(false);
    });
    cx.set_state("editorˇ");
    cx.simulate_keystroke(".");
    assert!(cx.editor(|e, _| e.context_menu.borrow_mut().is_none()));
    cx.simulate_keystroke("c");
    cx.simulate_keystroke("l");
    cx.simulate_keystroke("o");
    cx.assert_editor_state("editor.cloˇ");
    assert!(cx.editor(|e, _| e.context_menu.borrow_mut().is_none()));
    cx.update_editor(|editor, cx| {
        editor.show_completions(&ShowCompletions { trigger: None }, cx);
    });
    handle_completion_request(
        &mut cx,
        "editor.<clo|>",
        vec!["close", "clobber"],
        counter.clone(),
    )
    .await;
    cx.condition(|editor, _| editor.context_menu_visible())
        .await;
    assert_eq!(counter.load(atomic::Ordering::Acquire), 4);

    let apply_additional_edits = cx.update_editor(|editor, cx| {
        editor
            .confirm_completion(&ConfirmCompletion::default(), cx)
            .unwrap()
    });
    cx.assert_editor_state("editor.closeˇ");
    handle_resolve_completion_request(&mut cx, None).await;
    apply_additional_edits.await.unwrap();
}

#[gpui::test]
async fn test_completion_page_up_down_keys(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});
    let mut cx = EditorLspTestContext::new_rust(
        lsp::ServerCapabilities {
            completion_provider: Some(lsp::CompletionOptions {
                trigger_characters: Some(vec![".".to_string()]),
                ..Default::default()
            }),
            ..Default::default()
        },
        cx,
    )
    .await;
    cx.lsp
        .handle_request::<lsp::request::Completion, _, _>(move |_, _| async move {
            Ok(Some(lsp::CompletionResponse::Array(vec![
                lsp::CompletionItem {
                    label: "first".into(),
                    ..Default::default()
                },
                lsp::CompletionItem {
                    label: "last".into(),
                    ..Default::default()
                },
            ])))
        });
    cx.set_state("variableˇ");
    cx.simulate_keystroke(".");
    cx.executor().run_until_parked();

    cx.update_editor(|editor, _| {
        if let Some(CodeContextMenu::Completions(menu)) = editor.context_menu.borrow_mut().as_ref()
        {
            assert_eq!(completion_menu_entries(&menu.entries), &["first", "last"]);
        } else {
            panic!("expected completion menu to be open");
        }
    });

    cx.update_editor(|editor, cx| {
        editor.move_page_down(&MovePageDown::default(), cx);
        if let Some(CodeContextMenu::Completions(menu)) = editor.context_menu.borrow_mut().as_ref()
        {
            assert!(
                menu.selected_item == 1,
                "expected PageDown to select the last item from the context menu"
            );
        } else {
            panic!("expected completion menu to stay open after PageDown");
        }
    });

    cx.update_editor(|editor, cx| {
        editor.move_page_up(&MovePageUp::default(), cx);
        if let Some(CodeContextMenu::Completions(menu)) = editor.context_menu.borrow_mut().as_ref()
        {
            assert!(
                menu.selected_item == 0,
                "expected PageUp to select the first item from the context menu"
            );
        } else {
            panic!("expected completion menu to stay open after PageUp");
        }
    });
}

#[gpui::test]
async fn test_completion_sort(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});
    let mut cx = EditorLspTestContext::new_rust(
        lsp::ServerCapabilities {
            completion_provider: Some(lsp::CompletionOptions {
                trigger_characters: Some(vec![".".to_string()]),
                ..Default::default()
            }),
            ..Default::default()
        },
        cx,
    )
    .await;
    cx.lsp
        .handle_request::<lsp::request::Completion, _, _>(move |_, _| async move {
            Ok(Some(lsp::CompletionResponse::Array(vec![
                lsp::CompletionItem {
                    label: "Range".into(),
                    sort_text: Some("a".into()),
                    ..Default::default()
                },
                lsp::CompletionItem {
                    label: "r".into(),
                    sort_text: Some("b".into()),
                    ..Default::default()
                },
                lsp::CompletionItem {
                    label: "ret".into(),
                    sort_text: Some("c".into()),
                    ..Default::default()
                },
                lsp::CompletionItem {
                    label: "return".into(),
                    sort_text: Some("d".into()),
                    ..Default::default()
                },
                lsp::CompletionItem {
                    label: "slice".into(),
                    sort_text: Some("d".into()),
                    ..Default::default()
                },
            ])))
        });
    cx.set_state("rˇ");
    cx.executor().run_until_parked();
    cx.update_editor(|editor, cx| {
        editor.show_completions(
            &ShowCompletions {
                trigger: Some("r".into()),
            },
            cx,
        );
    });
    cx.executor().run_until_parked();

    cx.update_editor(|editor, _| {
        if let Some(CodeContextMenu::Completions(menu)) = editor.context_menu.borrow_mut().as_ref()
        {
            assert_eq!(
                completion_menu_entries(&menu.entries),
                &["r", "ret", "Range", "return"]
            );
        } else {
            panic!("expected completion menu to be open");
        }
    });
}

#[gpui::test]
async fn test_no_duplicated_completion_requests(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorLspTestContext::new_rust(
        lsp::ServerCapabilities {
            completion_provider: Some(lsp::CompletionOptions {
                trigger_characters: Some(vec![".".to_string()]),
                resolve_provider: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        },
        cx,
    )
    .await;

    cx.set_state(indoc! {"fn main() { let a = 2ˇ; }"});
    cx.simulate_keystroke(".");
    let completion_item = lsp::CompletionItem {
        label: "Some".into(),
        kind: Some(lsp::CompletionItemKind::SNIPPET),
        detail: Some("Wrap the expression in an `Option::Some`".to_string()),
        documentation: Some(lsp::Documentation::MarkupContent(lsp::MarkupContent {
            kind: lsp::MarkupKind::Markdown,
            value: "```rust\nSome(2)\n```".to_string(),
        })),
        deprecated: Some(false),
        sort_text: Some("Some".to_string()),
        filter_text: Some("Some".to_string()),
        insert_text_format: Some(lsp::InsertTextFormat::SNIPPET),
        text_edit: Some(lsp::CompletionTextEdit::Edit(lsp::TextEdit {
            range: lsp::Range {
                start: lsp::Position {
                    line: 0,
                    character: 22,
                },
                end: lsp::Position {
                    line: 0,
                    character: 22,
                },
            },
            new_text: "Some(2)".to_string(),
        })),
        additional_text_edits: Some(vec![lsp::TextEdit {
            range: lsp::Range {
                start: lsp::Position {
                    line: 0,
                    character: 20,
                },
                end: lsp::Position {
                    line: 0,
                    character: 22,
                },
            },
            new_text: "".to_string(),
        }]),
        ..Default::default()
    };

    let closure_completion_item = completion_item.clone();
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    let mut request = cx.handle_request::<lsp::request::Completion, _, _>(move |_, _, _| {
        let task_completion_item = closure_completion_item.clone();
        counter_clone.fetch_add(1, atomic::Ordering::Release);
        async move {
            Ok(Some(lsp::CompletionResponse::Array(vec![
                task_completion_item,
            ])))
        }
    });

    cx.condition(|editor, _| editor.context_menu_visible())
        .await;
    cx.assert_editor_state(indoc! {"fn main() { let a = 2.ˇ; }"});
    assert!(request.next().await.is_some());
    assert_eq!(counter.load(atomic::Ordering::Acquire), 1);

    cx.simulate_keystroke("S");
    cx.simulate_keystroke("o");
    cx.simulate_keystroke("m");
    cx.condition(|editor, _| editor.context_menu_visible())
        .await;
    cx.assert_editor_state(indoc! {"fn main() { let a = 2.Somˇ; }"});
    assert!(request.next().await.is_some());
    assert!(request.next().await.is_some());
    assert!(request.next().await.is_some());
    request.close();
    assert!(request.next().await.is_none());
    assert_eq!(
        counter.load(atomic::Ordering::Acquire),
        4,
        "With the completions menu open, only one LSP request should happen per input"
    );
}

#[gpui::test]
async fn test_toggle_comment(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});
    let mut cx = EditorTestContext::new(cx).await;
    let language = Arc::new(Language::new(
        LanguageConfig {
            line_comments: vec!["// ".into(), "//! ".into(), "/// ".into()],
            ..Default::default()
        },
        Some(tree_sitter_rust::LANGUAGE.into()),
    ));
    cx.update_buffer(|buffer, cx| buffer.set_language(Some(language), cx));

    // If multiple selections intersect a line, the line is only toggled once.
    cx.set_state(indoc! {"
        fn a() {
            «//b();
            ˇ»// «c();
            //ˇ»  d();
        }
    "});

    cx.update_editor(|e, cx| e.toggle_comments(&ToggleComments::default(), cx));

    cx.assert_editor_state(indoc! {"
        fn a() {
            «b();
            c();
            ˇ» d();
        }
    "});

    // The comment prefix is inserted at the same column for every line in a
    // selection.
    cx.update_editor(|e, cx| e.toggle_comments(&ToggleComments::default(), cx));

    cx.assert_editor_state(indoc! {"
        fn a() {
            // «b();
            // c();
            ˇ»//  d();
        }
    "});

    // If a selection ends at the beginning of a line, that line is not toggled.
    cx.set_selections_state(indoc! {"
        fn a() {
            // b();
            «// c();
        ˇ»    //  d();
        }
    "});

    cx.update_editor(|e, cx| e.toggle_comments(&ToggleComments::default(), cx));

    cx.assert_editor_state(indoc! {"
        fn a() {
            // b();
            «c();
        ˇ»    //  d();
        }
    "});

    // If a selection span a single line and is empty, the line is toggled.
    cx.set_state(indoc! {"
        fn a() {
            a();
            b();
        ˇ
        }
    "});

    cx.update_editor(|e, cx| e.toggle_comments(&ToggleComments::default(), cx));

    cx.assert_editor_state(indoc! {"
        fn a() {
            a();
            b();
        //•ˇ
        }
    "});

    // If a selection span multiple lines, empty lines are not toggled.
    cx.set_state(indoc! {"
        fn a() {
            «a();

            c();ˇ»
        }
    "});

    cx.update_editor(|e, cx| e.toggle_comments(&ToggleComments::default(), cx));

    cx.assert_editor_state(indoc! {"
        fn a() {
            // «a();

            // c();ˇ»
        }
    "});

    // If a selection includes multiple comment prefixes, all lines are uncommented.
    cx.set_state(indoc! {"
        fn a() {
            «// a();
            /// b();
            //! c();ˇ»
        }
    "});

    cx.update_editor(|e, cx| e.toggle_comments(&ToggleComments::default(), cx));

    cx.assert_editor_state(indoc! {"
        fn a() {
            «a();
            b();
            c();ˇ»
        }
    "});
}

#[gpui::test]
async fn test_toggle_comment_ignore_indent(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});
    let mut cx = EditorTestContext::new(cx).await;
    let language = Arc::new(Language::new(
        LanguageConfig {
            line_comments: vec!["// ".into(), "//! ".into(), "/// ".into()],
            ..Default::default()
        },
        Some(tree_sitter_rust::LANGUAGE.into()),
    ));
    cx.update_buffer(|buffer, cx| buffer.set_language(Some(language), cx));

    let toggle_comments = &ToggleComments {
        advance_downwards: false,
        ignore_indent: true,
    };

    // If multiple selections intersect a line, the line is only toggled once.
    cx.set_state(indoc! {"
        fn a() {
        //    «b();
        //    c();
        //    ˇ» d();
        }
    "});

    cx.update_editor(|e, cx| e.toggle_comments(toggle_comments, cx));

    cx.assert_editor_state(indoc! {"
        fn a() {
            «b();
            c();
            ˇ» d();
        }
    "});

    // The comment prefix is inserted at the beginning of each line
    cx.update_editor(|e, cx| e.toggle_comments(toggle_comments, cx));

    cx.assert_editor_state(indoc! {"
        fn a() {
        //    «b();
        //    c();
        //    ˇ» d();
        }
    "});

    // If a selection ends at the beginning of a line, that line is not toggled.
    cx.set_selections_state(indoc! {"
        fn a() {
        //    b();
        //    «c();
        ˇ»//     d();
        }
    "});

    cx.update_editor(|e, cx| e.toggle_comments(toggle_comments, cx));

    cx.assert_editor_state(indoc! {"
        fn a() {
        //    b();
            «c();
        ˇ»//     d();
        }
    "});

    // If a selection span a single line and is empty, the line is toggled.
    cx.set_state(indoc! {"
        fn a() {
            a();
            b();
        ˇ
        }
    "});

    cx.update_editor(|e, cx| e.toggle_comments(toggle_comments, cx));

    cx.assert_editor_state(indoc! {"
        fn a() {
            a();
            b();
        //ˇ
        }
    "});

    // If a selection span multiple lines, empty lines are not toggled.
    cx.set_state(indoc! {"
        fn a() {
            «a();

            c();ˇ»
        }
    "});

    cx.update_editor(|e, cx| e.toggle_comments(toggle_comments, cx));

    cx.assert_editor_state(indoc! {"
        fn a() {
        //    «a();

        //    c();ˇ»
        }
    "});

    // If a selection includes multiple comment prefixes, all lines are uncommented.
    cx.set_state(indoc! {"
        fn a() {
        //    «a();
        ///    b();
        //!    c();ˇ»
        }
    "});

    cx.update_editor(|e, cx| e.toggle_comments(toggle_comments, cx));

    cx.assert_editor_state(indoc! {"
        fn a() {
            «a();
            b();
            c();ˇ»
        }
    "});
}

#[gpui::test]
async fn test_advance_downward_on_toggle_comment(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let language = Arc::new(Language::new(
        LanguageConfig {
            line_comments: vec!["// ".into()],
            ..Default::default()
        },
        Some(tree_sitter_rust::LANGUAGE.into()),
    ));

    let mut cx = EditorTestContext::new(cx).await;

    cx.language_registry().add(language.clone());
    cx.update_buffer(|buffer, cx| {
        buffer.set_language(Some(language), cx);
    });

    let toggle_comments = &ToggleComments {
        advance_downwards: true,
        ignore_indent: false,
    };

    // Single cursor on one line -> advance
    // Cursor moves horizontally 3 characters as well on non-blank line
    cx.set_state(indoc!(
        "fn a() {
             ˇdog();
             cat();
        }"
    ));
    cx.update_editor(|editor, cx| {
        editor.toggle_comments(toggle_comments, cx);
    });
    cx.assert_editor_state(indoc!(
        "fn a() {
             // dog();
             catˇ();
        }"
    ));

    // Single selection on one line -> don't advance
    cx.set_state(indoc!(
        "fn a() {
             «dog()ˇ»;
             cat();
        }"
    ));
    cx.update_editor(|editor, cx| {
        editor.toggle_comments(toggle_comments, cx);
    });
    cx.assert_editor_state(indoc!(
        "fn a() {
             // «dog()ˇ»;
             cat();
        }"
    ));

    // Multiple cursors on one line -> advance
    cx.set_state(indoc!(
        "fn a() {
             ˇdˇog();
             cat();
        }"
    ));
    cx.update_editor(|editor, cx| {
        editor.toggle_comments(toggle_comments, cx);
    });
    cx.assert_editor_state(indoc!(
        "fn a() {
             // dog();
             catˇ(ˇ);
        }"
    ));

    // Multiple cursors on one line, with selection -> don't advance
    cx.set_state(indoc!(
        "fn a() {
             ˇdˇog«()ˇ»;
             cat();
        }"
    ));
    cx.update_editor(|editor, cx| {
        editor.toggle_comments(toggle_comments, cx);
    });
    cx.assert_editor_state(indoc!(
        "fn a() {
             // ˇdˇog«()ˇ»;
             cat();
        }"
    ));

    // Single cursor on one line -> advance
    // Cursor moves to column 0 on blank line
    cx.set_state(indoc!(
        "fn a() {
             ˇdog();

             cat();
        }"
    ));
    cx.update_editor(|editor, cx| {
        editor.toggle_comments(toggle_comments, cx);
    });
    cx.assert_editor_state(indoc!(
        "fn a() {
             // dog();
        ˇ
             cat();
        }"
    ));

    // Single cursor on one line -> advance
    // Cursor starts and ends at column 0
    cx.set_state(indoc!(
        "fn a() {
         ˇ    dog();
             cat();
        }"
    ));
    cx.update_editor(|editor, cx| {
        editor.toggle_comments(toggle_comments, cx);
    });
    cx.assert_editor_state(indoc!(
        "fn a() {
             // dog();
         ˇ    cat();
        }"
    ));
}

#[gpui::test]
async fn test_toggle_block_comment(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;

    let html_language = Arc::new(
        Language::new(
            LanguageConfig {
                name: "HTML".into(),
                block_comment: Some(("<!-- ".into(), " -->".into())),
                ..Default::default()
            },
            Some(tree_sitter_html::language()),
        )
        .with_injection_query(
            r#"
            (script_element
                (raw_text) @content
                (#set! "language" "javascript"))
            "#,
        )
        .unwrap(),
    );

    let javascript_language = Arc::new(Language::new(
        LanguageConfig {
            name: "JavaScript".into(),
            line_comments: vec!["// ".into()],
            ..Default::default()
        },
        Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
    ));

    cx.language_registry().add(html_language.clone());
    cx.language_registry().add(javascript_language.clone());
    cx.update_buffer(|buffer, cx| {
        buffer.set_language(Some(html_language), cx);
    });

    // Toggle comments for empty selections
    cx.set_state(
        &r#"
            <p>A</p>ˇ
            <p>B</p>ˇ
            <p>C</p>ˇ
        "#
        .unindent(),
    );
    cx.update_editor(|editor, cx| editor.toggle_comments(&ToggleComments::default(), cx));
    cx.assert_editor_state(
        &r#"
            <!-- <p>A</p>ˇ -->
            <!-- <p>B</p>ˇ -->
            <!-- <p>C</p>ˇ -->
        "#
        .unindent(),
    );
    cx.update_editor(|editor, cx| editor.toggle_comments(&ToggleComments::default(), cx));
    cx.assert_editor_state(
        &r#"
            <p>A</p>ˇ
            <p>B</p>ˇ
            <p>C</p>ˇ
        "#
        .unindent(),
    );

    // Toggle comments for mixture of empty and non-empty selections, where
    // multiple selections occupy a given line.
    cx.set_state(
        &r#"
            <p>A«</p>
            <p>ˇ»B</p>ˇ
            <p>C«</p>
            <p>ˇ»D</p>ˇ
        "#
        .unindent(),
    );

    cx.update_editor(|editor, cx| editor.toggle_comments(&ToggleComments::default(), cx));
    cx.assert_editor_state(
        &r#"
            <!-- <p>A«</p>
            <p>ˇ»B</p>ˇ -->
            <!-- <p>C«</p>
            <p>ˇ»D</p>ˇ -->
        "#
        .unindent(),
    );
    cx.update_editor(|editor, cx| editor.toggle_comments(&ToggleComments::default(), cx));
    cx.assert_editor_state(
        &r#"
            <p>A«</p>
            <p>ˇ»B</p>ˇ
            <p>C«</p>
            <p>ˇ»D</p>ˇ
        "#
        .unindent(),
    );

    // Toggle comments when different languages are active for different
    // selections.
    cx.set_state(
        &r#"
            ˇ<script>
                ˇvar x = new Y();
            ˇ</script>
        "#
        .unindent(),
    );
    cx.executor().run_until_parked();
    cx.update_editor(|editor, cx| editor.toggle_comments(&ToggleComments::default(), cx));
    // TODO this is how it actually worked in Zed Stable, which is not very ergonomic.
    // Uncommenting and commenting from this position brings in even more wrong artifacts.
    cx.assert_editor_state(
        &r#"
            <!-- ˇ<script> -->
                // ˇvar x = new Y();
            // ˇ</script>
        "#
        .unindent(),
    );
}

#[gpui::test]
fn test_editing_disjoint_excerpts(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let buffer = cx.new_model(|cx| Buffer::local(sample_text(3, 4, 'a'), cx));
    let multibuffer = cx.new_model(|cx| {
        let mut multibuffer = MultiBuffer::new(ReadWrite);
        multibuffer.push_excerpts(
            buffer.clone(),
            [
                ExcerptRange {
                    context: Point::new(0, 0)..Point::new(0, 4),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(1, 0)..Point::new(1, 4),
                    primary: None,
                },
            ],
            cx,
        );
        assert_eq!(multibuffer.read(cx).text(), "aaaa\nbbbb");
        multibuffer
    });

    let (view, cx) = cx.add_window_view(|cx| build_editor(multibuffer, cx));
    view.update(cx, |view, cx| {
        assert_eq!(view.text(cx), "aaaa\nbbbb");
        view.change_selections(None, cx, |s| {
            s.select_ranges([
                Point::new(0, 0)..Point::new(0, 0),
                Point::new(1, 0)..Point::new(1, 0),
            ])
        });

        view.handle_input("X", cx);
        assert_eq!(view.text(cx), "Xaaaa\nXbbbb");
        assert_eq!(
            view.selections.ranges(cx),
            [
                Point::new(0, 1)..Point::new(0, 1),
                Point::new(1, 1)..Point::new(1, 1),
            ]
        );

        // Ensure the cursor's head is respected when deleting across an excerpt boundary.
        view.change_selections(None, cx, |s| {
            s.select_ranges([Point::new(0, 2)..Point::new(1, 2)])
        });
        view.backspace(&Default::default(), cx);
        assert_eq!(view.text(cx), "Xa\nbbb");
        assert_eq!(
            view.selections.ranges(cx),
            [Point::new(1, 0)..Point::new(1, 0)]
        );

        view.change_selections(None, cx, |s| {
            s.select_ranges([Point::new(1, 1)..Point::new(0, 1)])
        });
        view.backspace(&Default::default(), cx);
        assert_eq!(view.text(cx), "X\nbb");
        assert_eq!(
            view.selections.ranges(cx),
            [Point::new(0, 1)..Point::new(0, 1)]
        );
    });
}

#[gpui::test]
fn test_editing_overlapping_excerpts(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let markers = vec![('[', ']').into(), ('(', ')').into()];
    let (initial_text, mut excerpt_ranges) = marked_text_ranges_by(
        indoc! {"
            [aaaa
            (bbbb]
            cccc)",
        },
        markers.clone(),
    );
    let excerpt_ranges = markers.into_iter().map(|marker| {
        let context = excerpt_ranges.remove(&marker).unwrap()[0].clone();
        ExcerptRange {
            context,
            primary: None,
        }
    });
    let buffer = cx.new_model(|cx| Buffer::local(initial_text, cx));
    let multibuffer = cx.new_model(|cx| {
        let mut multibuffer = MultiBuffer::new(ReadWrite);
        multibuffer.push_excerpts(buffer, excerpt_ranges, cx);
        multibuffer
    });

    let (view, cx) = cx.add_window_view(|cx| build_editor(multibuffer, cx));
    view.update(cx, |view, cx| {
        let (expected_text, selection_ranges) = marked_text_ranges(
            indoc! {"
                aaaa
                bˇbbb
                bˇbbˇb
                cccc"
            },
            true,
        );
        assert_eq!(view.text(cx), expected_text);
        view.change_selections(None, cx, |s| s.select_ranges(selection_ranges));

        view.handle_input("X", cx);

        let (expected_text, expected_selections) = marked_text_ranges(
            indoc! {"
                aaaa
                bXˇbbXb
                bXˇbbXˇb
                cccc"
            },
            false,
        );
        assert_eq!(view.text(cx), expected_text);
        assert_eq!(view.selections.ranges(cx), expected_selections);

        view.newline(&Newline, cx);
        let (expected_text, expected_selections) = marked_text_ranges(
            indoc! {"
                aaaa
                bX
                ˇbbX
                b
                bX
                ˇbbX
                ˇb
                cccc"
            },
            false,
        );
        assert_eq!(view.text(cx), expected_text);
        assert_eq!(view.selections.ranges(cx), expected_selections);
    });
}

#[gpui::test]
fn test_refresh_selections(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let buffer = cx.new_model(|cx| Buffer::local(sample_text(3, 4, 'a'), cx));
    let mut excerpt1_id = None;
    let multibuffer = cx.new_model(|cx| {
        let mut multibuffer = MultiBuffer::new(ReadWrite);
        excerpt1_id = multibuffer
            .push_excerpts(
                buffer.clone(),
                [
                    ExcerptRange {
                        context: Point::new(0, 0)..Point::new(1, 4),
                        primary: None,
                    },
                    ExcerptRange {
                        context: Point::new(1, 0)..Point::new(2, 4),
                        primary: None,
                    },
                ],
                cx,
            )
            .into_iter()
            .next();
        assert_eq!(multibuffer.read(cx).text(), "aaaa\nbbbb\nbbbb\ncccc");
        multibuffer
    });

    let editor = cx.add_window(|cx| {
        let mut editor = build_editor(multibuffer.clone(), cx);
        let snapshot = editor.snapshot(cx);
        editor.change_selections(None, cx, |s| {
            s.select_ranges([Point::new(1, 3)..Point::new(1, 3)])
        });
        editor.begin_selection(Point::new(2, 1).to_display_point(&snapshot), true, 1, cx);
        assert_eq!(
            editor.selections.ranges(cx),
            [
                Point::new(1, 3)..Point::new(1, 3),
                Point::new(2, 1)..Point::new(2, 1),
            ]
        );
        editor
    });

    // Refreshing selections is a no-op when excerpts haven't changed.
    _ = editor.update(cx, |editor, cx| {
        editor.change_selections(None, cx, |s| s.refresh());
        assert_eq!(
            editor.selections.ranges(cx),
            [
                Point::new(1, 3)..Point::new(1, 3),
                Point::new(2, 1)..Point::new(2, 1),
            ]
        );
    });

    multibuffer.update(cx, |multibuffer, cx| {
        multibuffer.remove_excerpts([excerpt1_id.unwrap()], cx);
    });
    _ = editor.update(cx, |editor, cx| {
        // Removing an excerpt causes the first selection to become degenerate.
        assert_eq!(
            editor.selections.ranges(cx),
            [
                Point::new(0, 0)..Point::new(0, 0),
                Point::new(0, 1)..Point::new(0, 1)
            ]
        );

        // Refreshing selections will relocate the first selection to the original buffer
        // location.
        editor.change_selections(None, cx, |s| s.refresh());
        assert_eq!(
            editor.selections.ranges(cx),
            [
                Point::new(0, 1)..Point::new(0, 1),
                Point::new(0, 3)..Point::new(0, 3)
            ]
        );
        assert!(editor.selections.pending_anchor().is_some());
    });
}

#[gpui::test]
fn test_refresh_selections_while_selecting_with_mouse(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let buffer = cx.new_model(|cx| Buffer::local(sample_text(3, 4, 'a'), cx));
    let mut excerpt1_id = None;
    let multibuffer = cx.new_model(|cx| {
        let mut multibuffer = MultiBuffer::new(ReadWrite);
        excerpt1_id = multibuffer
            .push_excerpts(
                buffer.clone(),
                [
                    ExcerptRange {
                        context: Point::new(0, 0)..Point::new(1, 4),
                        primary: None,
                    },
                    ExcerptRange {
                        context: Point::new(1, 0)..Point::new(2, 4),
                        primary: None,
                    },
                ],
                cx,
            )
            .into_iter()
            .next();
        assert_eq!(multibuffer.read(cx).text(), "aaaa\nbbbb\nbbbb\ncccc");
        multibuffer
    });

    let editor = cx.add_window(|cx| {
        let mut editor = build_editor(multibuffer.clone(), cx);
        let snapshot = editor.snapshot(cx);
        editor.begin_selection(Point::new(1, 3).to_display_point(&snapshot), false, 1, cx);
        assert_eq!(
            editor.selections.ranges(cx),
            [Point::new(1, 3)..Point::new(1, 3)]
        );
        editor
    });

    multibuffer.update(cx, |multibuffer, cx| {
        multibuffer.remove_excerpts([excerpt1_id.unwrap()], cx);
    });
    _ = editor.update(cx, |editor, cx| {
        assert_eq!(
            editor.selections.ranges(cx),
            [Point::new(0, 0)..Point::new(0, 0)]
        );

        // Ensure we don't panic when selections are refreshed and that the pending selection is finalized.
        editor.change_selections(None, cx, |s| s.refresh());
        assert_eq!(
            editor.selections.ranges(cx),
            [Point::new(0, 3)..Point::new(0, 3)]
        );
        assert!(editor.selections.pending_anchor().is_some());
    });
}

#[gpui::test]
async fn test_extra_newline_insertion(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let language = Arc::new(
        Language::new(
            LanguageConfig {
                brackets: BracketPairConfig {
                    pairs: vec![
                        BracketPair {
                            start: "{".to_string(),
                            end: "}".to_string(),
                            close: true,
                            surround: true,
                            newline: true,
                        },
                        BracketPair {
                            start: "/* ".to_string(),
                            end: " */".to_string(),
                            close: true,
                            surround: true,
                            newline: true,
                        },
                    ],
                    ..Default::default()
                },
                ..Default::default()
            },
            Some(tree_sitter_rust::LANGUAGE.into()),
        )
        .with_indents_query("")
        .unwrap(),
    );

    let text = concat!(
        "{   }\n",     //
        "  x\n",       //
        "  /*   */\n", //
        "x\n",         //
        "{{} }\n",     //
    );

    let buffer = cx.new_model(|cx| Buffer::local(text, cx).with_language(language, cx));
    let buffer = cx.new_model(|cx| MultiBuffer::singleton(buffer, cx));
    let (view, cx) = cx.add_window_view(|cx| build_editor(buffer, cx));
    view.condition::<crate::EditorEvent>(cx, |view, cx| !view.buffer.read(cx).is_parsing(cx))
        .await;

    view.update(cx, |view, cx| {
        view.change_selections(None, cx, |s| {
            s.select_display_ranges([
                DisplayPoint::new(DisplayRow(0), 2)..DisplayPoint::new(DisplayRow(0), 3),
                DisplayPoint::new(DisplayRow(2), 5)..DisplayPoint::new(DisplayRow(2), 5),
                DisplayPoint::new(DisplayRow(4), 4)..DisplayPoint::new(DisplayRow(4), 4),
            ])
        });
        view.newline(&Newline, cx);

        assert_eq!(
            view.buffer().read(cx).read(cx).text(),
            concat!(
                "{ \n",    // Suppress rustfmt
                "\n",      //
                "}\n",     //
                "  x\n",   //
                "  /* \n", //
                "  \n",    //
                "  */\n",  //
                "x\n",     //
                "{{} \n",  //
                "}\n",     //
            )
        );
    });
}

#[gpui::test]
fn test_highlighted_ranges(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let editor = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple(&sample_text(16, 8, 'a'), cx);
        build_editor(buffer.clone(), cx)
    });

    _ = editor.update(cx, |editor, cx| {
        struct Type1;
        struct Type2;

        let buffer = editor.buffer.read(cx).snapshot(cx);

        let anchor_range =
            |range: Range<Point>| buffer.anchor_after(range.start)..buffer.anchor_after(range.end);

        editor.highlight_background::<Type1>(
            &[
                anchor_range(Point::new(2, 1)..Point::new(2, 3)),
                anchor_range(Point::new(4, 2)..Point::new(4, 4)),
                anchor_range(Point::new(6, 3)..Point::new(6, 5)),
                anchor_range(Point::new(8, 4)..Point::new(8, 6)),
            ],
            |_| Hsla::red(),
            cx,
        );
        editor.highlight_background::<Type2>(
            &[
                anchor_range(Point::new(3, 2)..Point::new(3, 5)),
                anchor_range(Point::new(5, 3)..Point::new(5, 6)),
                anchor_range(Point::new(7, 4)..Point::new(7, 7)),
                anchor_range(Point::new(9, 5)..Point::new(9, 8)),
            ],
            |_| Hsla::green(),
            cx,
        );

        let snapshot = editor.snapshot(cx);
        let mut highlighted_ranges = editor.background_highlights_in_range(
            anchor_range(Point::new(3, 4)..Point::new(7, 4)),
            &snapshot,
            cx.theme().colors(),
        );
        // Enforce a consistent ordering based on color without relying on the ordering of the
        // highlight's `TypeId` which is non-executor.
        highlighted_ranges.sort_unstable_by_key(|(_, color)| *color);
        assert_eq!(
            highlighted_ranges,
            &[
                (
                    DisplayPoint::new(DisplayRow(4), 2)..DisplayPoint::new(DisplayRow(4), 4),
                    Hsla::red(),
                ),
                (
                    DisplayPoint::new(DisplayRow(6), 3)..DisplayPoint::new(DisplayRow(6), 5),
                    Hsla::red(),
                ),
                (
                    DisplayPoint::new(DisplayRow(3), 2)..DisplayPoint::new(DisplayRow(3), 5),
                    Hsla::green(),
                ),
                (
                    DisplayPoint::new(DisplayRow(5), 3)..DisplayPoint::new(DisplayRow(5), 6),
                    Hsla::green(),
                ),
            ]
        );
        assert_eq!(
            editor.background_highlights_in_range(
                anchor_range(Point::new(5, 6)..Point::new(6, 4)),
                &snapshot,
                cx.theme().colors(),
            ),
            &[(
                DisplayPoint::new(DisplayRow(6), 3)..DisplayPoint::new(DisplayRow(6), 5),
                Hsla::red(),
            )]
        );
    });
}

#[gpui::test]
async fn test_following(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let fs = FakeFs::new(cx.executor());
    let project = Project::test(fs, ["/file.rs".as_ref()], cx).await;

    let buffer = project.update(cx, |project, cx| {
        let buffer = project.create_local_buffer(&sample_text(16, 8, 'a'), None, cx);
        cx.new_model(|cx| MultiBuffer::singleton(buffer, cx))
    });
    let leader = cx.add_window(|cx| build_editor(buffer.clone(), cx));
    let follower = cx.update(|cx| {
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::from_corners(
                    gpui::Point::new(px(0.), px(0.)),
                    gpui::Point::new(px(10.), px(80.)),
                ))),
                ..Default::default()
            },
            |cx| cx.new_view(|cx| build_editor(buffer.clone(), cx)),
        )
        .unwrap()
    });

    let is_still_following = Rc::new(RefCell::new(true));
    let follower_edit_event_count = Rc::new(RefCell::new(0));
    let pending_update = Rc::new(RefCell::new(None));
    _ = follower.update(cx, {
        let update = pending_update.clone();
        let is_still_following = is_still_following.clone();
        let follower_edit_event_count = follower_edit_event_count.clone();
        |_, cx| {
            cx.subscribe(
                &leader.root_view(cx).unwrap(),
                move |_, leader, event, cx| {
                    leader
                        .read(cx)
                        .add_event_to_update_proto(event, &mut update.borrow_mut(), cx);
                },
            )
            .detach();

            cx.subscribe(
                &follower.root_view(cx).unwrap(),
                move |_, _, event: &EditorEvent, _cx| {
                    if matches!(Editor::to_follow_event(event), Some(FollowEvent::Unfollow)) {
                        *is_still_following.borrow_mut() = false;
                    }

                    if let EditorEvent::BufferEdited = event {
                        *follower_edit_event_count.borrow_mut() += 1;
                    }
                },
            )
            .detach();
        }
    });

    // Update the selections only
    _ = leader.update(cx, |leader, cx| {
        leader.change_selections(None, cx, |s| s.select_ranges([1..1]));
    });
    follower
        .update(cx, |follower, cx| {
            follower.apply_update_proto(&project, pending_update.borrow_mut().take().unwrap(), cx)
        })
        .unwrap()
        .await
        .unwrap();
    _ = follower.update(cx, |follower, cx| {
        assert_eq!(follower.selections.ranges(cx), vec![1..1]);
    });
    assert!(*is_still_following.borrow());
    assert_eq!(*follower_edit_event_count.borrow(), 0);

    // Update the scroll position only
    _ = leader.update(cx, |leader, cx| {
        leader.set_scroll_position(gpui::Point::new(1.5, 3.5), cx);
    });
    follower
        .update(cx, |follower, cx| {
            follower.apply_update_proto(&project, pending_update.borrow_mut().take().unwrap(), cx)
        })
        .unwrap()
        .await
        .unwrap();
    assert_eq!(
        follower
            .update(cx, |follower, cx| follower.scroll_position(cx))
            .unwrap(),
        gpui::Point::new(1.5, 3.5)
    );
    assert!(*is_still_following.borrow());
    assert_eq!(*follower_edit_event_count.borrow(), 0);

    // Update the selections and scroll position. The follower's scroll position is updated
    // via autoscroll, not via the leader's exact scroll position.
    _ = leader.update(cx, |leader, cx| {
        leader.change_selections(None, cx, |s| s.select_ranges([0..0]));
        leader.request_autoscroll(Autoscroll::newest(), cx);
        leader.set_scroll_position(gpui::Point::new(1.5, 3.5), cx);
    });
    follower
        .update(cx, |follower, cx| {
            follower.apply_update_proto(&project, pending_update.borrow_mut().take().unwrap(), cx)
        })
        .unwrap()
        .await
        .unwrap();
    _ = follower.update(cx, |follower, cx| {
        assert_eq!(follower.scroll_position(cx), gpui::Point::new(1.5, 0.0));
        assert_eq!(follower.selections.ranges(cx), vec![0..0]);
    });
    assert!(*is_still_following.borrow());

    // Creating a pending selection that precedes another selection
    _ = leader.update(cx, |leader, cx| {
        leader.change_selections(None, cx, |s| s.select_ranges([1..1]));
        leader.begin_selection(DisplayPoint::new(DisplayRow(0), 0), true, 1, cx);
    });
    follower
        .update(cx, |follower, cx| {
            follower.apply_update_proto(&project, pending_update.borrow_mut().take().unwrap(), cx)
        })
        .unwrap()
        .await
        .unwrap();
    _ = follower.update(cx, |follower, cx| {
        assert_eq!(follower.selections.ranges(cx), vec![0..0, 1..1]);
    });
    assert!(*is_still_following.borrow());

    // Extend the pending selection so that it surrounds another selection
    _ = leader.update(cx, |leader, cx| {
        leader.extend_selection(DisplayPoint::new(DisplayRow(0), 2), 1, cx);
    });
    follower
        .update(cx, |follower, cx| {
            follower.apply_update_proto(&project, pending_update.borrow_mut().take().unwrap(), cx)
        })
        .unwrap()
        .await
        .unwrap();
    _ = follower.update(cx, |follower, cx| {
        assert_eq!(follower.selections.ranges(cx), vec![0..2]);
    });

    // Scrolling locally breaks the follow
    _ = follower.update(cx, |follower, cx| {
        let top_anchor = follower.buffer().read(cx).read(cx).anchor_after(0);
        follower.set_scroll_anchor(
            ScrollAnchor {
                anchor: top_anchor,
                offset: gpui::Point::new(0.0, 0.5),
            },
            cx,
        );
    });
    assert!(!(*is_still_following.borrow()));
}

#[gpui::test]
async fn test_following_with_multiple_excerpts(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let fs = FakeFs::new(cx.executor());
    let project = Project::test(fs, ["/file.rs".as_ref()], cx).await;
    let workspace = cx.add_window(|cx| Workspace::test_new(project.clone(), cx));
    let pane = workspace
        .update(cx, |workspace, _| workspace.active_pane().clone())
        .unwrap();

    let cx = &mut VisualTestContext::from_window(*workspace.deref(), cx);

    let leader = pane.update(cx, |_, cx| {
        let multibuffer = cx.new_model(|_| MultiBuffer::new(ReadWrite));
        cx.new_view(|cx| build_editor(multibuffer.clone(), cx))
    });

    // Start following the editor when it has no excerpts.
    let mut state_message = leader.update(cx, |leader, cx| leader.to_state_proto(cx));
    let follower_1 = cx
        .update_window(*workspace.deref(), |_, cx| {
            Editor::from_state_proto(
                workspace.root_view(cx).unwrap(),
                ViewId {
                    creator: Default::default(),
                    id: 0,
                },
                &mut state_message,
                cx,
            )
        })
        .unwrap()
        .unwrap()
        .await
        .unwrap();

    let update_message = Rc::new(RefCell::new(None));
    follower_1.update(cx, {
        let update = update_message.clone();
        |_, cx| {
            cx.subscribe(&leader, move |_, leader, event, cx| {
                leader
                    .read(cx)
                    .add_event_to_update_proto(event, &mut update.borrow_mut(), cx);
            })
            .detach();
        }
    });

    let (buffer_1, buffer_2) = project.update(cx, |project, cx| {
        (
            project.create_local_buffer("abc\ndef\nghi\njkl\n", None, cx),
            project.create_local_buffer("mno\npqr\nstu\nvwx\n", None, cx),
        )
    });

    // Insert some excerpts.
    leader.update(cx, |leader, cx| {
        leader.buffer.update(cx, |multibuffer, cx| {
            let excerpt_ids = multibuffer.push_excerpts(
                buffer_1.clone(),
                [
                    ExcerptRange {
                        context: 1..6,
                        primary: None,
                    },
                    ExcerptRange {
                        context: 12..15,
                        primary: None,
                    },
                    ExcerptRange {
                        context: 0..3,
                        primary: None,
                    },
                ],
                cx,
            );
            multibuffer.insert_excerpts_after(
                excerpt_ids[0],
                buffer_2.clone(),
                [
                    ExcerptRange {
                        context: 8..12,
                        primary: None,
                    },
                    ExcerptRange {
                        context: 0..6,
                        primary: None,
                    },
                ],
                cx,
            );
        });
    });

    // Apply the update of adding the excerpts.
    follower_1
        .update(cx, |follower, cx| {
            follower.apply_update_proto(&project, update_message.borrow().clone().unwrap(), cx)
        })
        .await
        .unwrap();
    assert_eq!(
        follower_1.update(cx, |editor, cx| editor.text(cx)),
        leader.update(cx, |editor, cx| editor.text(cx))
    );
    update_message.borrow_mut().take();

    // Start following separately after it already has excerpts.
    let mut state_message = leader.update(cx, |leader, cx| leader.to_state_proto(cx));
    let follower_2 = cx
        .update_window(*workspace.deref(), |_, cx| {
            Editor::from_state_proto(
                workspace.root_view(cx).unwrap().clone(),
                ViewId {
                    creator: Default::default(),
                    id: 0,
                },
                &mut state_message,
                cx,
            )
        })
        .unwrap()
        .unwrap()
        .await
        .unwrap();
    assert_eq!(
        follower_2.update(cx, |editor, cx| editor.text(cx)),
        leader.update(cx, |editor, cx| editor.text(cx))
    );

    // Remove some excerpts.
    leader.update(cx, |leader, cx| {
        leader.buffer.update(cx, |multibuffer, cx| {
            let excerpt_ids = multibuffer.excerpt_ids();
            multibuffer.remove_excerpts([excerpt_ids[1], excerpt_ids[2]], cx);
            multibuffer.remove_excerpts([excerpt_ids[0]], cx);
        });
    });

    // Apply the update of removing the excerpts.
    follower_1
        .update(cx, |follower, cx| {
            follower.apply_update_proto(&project, update_message.borrow().clone().unwrap(), cx)
        })
        .await
        .unwrap();
    follower_2
        .update(cx, |follower, cx| {
            follower.apply_update_proto(&project, update_message.borrow().clone().unwrap(), cx)
        })
        .await
        .unwrap();
    update_message.borrow_mut().take();
    assert_eq!(
        follower_1.update(cx, |editor, cx| editor.text(cx)),
        leader.update(cx, |editor, cx| editor.text(cx))
    );
}

#[gpui::test]
async fn go_to_prev_overlapping_diagnostic(
    executor: BackgroundExecutor,
    cx: &mut gpui::TestAppContext,
) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;
    let lsp_store =
        cx.update_editor(|editor, cx| editor.project.as_ref().unwrap().read(cx).lsp_store());

    cx.set_state(indoc! {"
        ˇfn func(abc def: i32) -> u32 {
        }
    "});

    cx.update(|cx| {
        lsp_store.update(cx, |lsp_store, cx| {
            lsp_store
                .update_diagnostics(
                    LanguageServerId(0),
                    lsp::PublishDiagnosticsParams {
                        uri: lsp::Url::from_file_path("/root/file").unwrap(),
                        version: None,
                        diagnostics: vec![
                            lsp::Diagnostic {
                                range: lsp::Range::new(
                                    lsp::Position::new(0, 11),
                                    lsp::Position::new(0, 12),
                                ),
                                severity: Some(lsp::DiagnosticSeverity::ERROR),
                                ..Default::default()
                            },
                            lsp::Diagnostic {
                                range: lsp::Range::new(
                                    lsp::Position::new(0, 12),
                                    lsp::Position::new(0, 15),
                                ),
                                severity: Some(lsp::DiagnosticSeverity::ERROR),
                                ..Default::default()
                            },
                            lsp::Diagnostic {
                                range: lsp::Range::new(
                                    lsp::Position::new(0, 25),
                                    lsp::Position::new(0, 28),
                                ),
                                severity: Some(lsp::DiagnosticSeverity::ERROR),
                                ..Default::default()
                            },
                        ],
                    },
                    &[],
                    cx,
                )
                .unwrap()
        });
    });

    executor.run_until_parked();

    cx.update_editor(|editor, cx| {
        editor.go_to_prev_diagnostic(&GoToPrevDiagnostic, cx);
    });

    cx.assert_editor_state(indoc! {"
        fn func(abc def: i32) -> ˇu32 {
        }
    "});

    cx.update_editor(|editor, cx| {
        editor.go_to_prev_diagnostic(&GoToPrevDiagnostic, cx);
    });

    cx.assert_editor_state(indoc! {"
        fn func(abc ˇdef: i32) -> u32 {
        }
    "});

    cx.update_editor(|editor, cx| {
        editor.go_to_prev_diagnostic(&GoToPrevDiagnostic, cx);
    });

    cx.assert_editor_state(indoc! {"
        fn func(abcˇ def: i32) -> u32 {
        }
    "});

    cx.update_editor(|editor, cx| {
        editor.go_to_prev_diagnostic(&GoToPrevDiagnostic, cx);
    });

    cx.assert_editor_state(indoc! {"
        fn func(abc def: i32) -> ˇu32 {
        }
    "});
}

#[gpui::test]
async fn test_diagnostics_with_links(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;

    cx.set_state(indoc! {"
        fn func(abˇc def: i32) -> u32 {
        }
    "});
    let lsp_store =
        cx.update_editor(|editor, cx| editor.project.as_ref().unwrap().read(cx).lsp_store());

    cx.update(|cx| {
        lsp_store.update(cx, |lsp_store, cx| {
            lsp_store.update_diagnostics(
                LanguageServerId(0),
                lsp::PublishDiagnosticsParams {
                    uri: lsp::Url::from_file_path("/root/file").unwrap(),
                    version: None,
                    diagnostics: vec![lsp::Diagnostic {
                        range: lsp::Range::new(lsp::Position::new(0, 8), lsp::Position::new(0, 12)),
                        severity: Some(lsp::DiagnosticSeverity::ERROR),
                        message: "we've had problems with <https://link.one>, and <https://link.two> is broken".to_string(),
                        ..Default::default()
                    }],
                },
                &[],
                cx,
            )
        })
    }).unwrap();
    cx.run_until_parked();
    cx.update_editor(|editor, cx| hover_popover::hover(editor, &Default::default(), cx));
    cx.run_until_parked();
    cx.update_editor(|editor, _| assert!(editor.hover_state.diagnostic_popover.is_some()))
}

#[gpui::test]
async fn go_to_hunk(executor: BackgroundExecutor, cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;

    let diff_base = r#"
        use some::mod;

        const A: u32 = 42;

        fn main() {
            println!("hello");

            println!("world");
        }
        "#
    .unindent();

    // Edits are modified, removed, modified, added
    cx.set_state(
        &r#"
        use some::modified;

        ˇ
        fn main() {
            println!("hello there");

            println!("around the");
            println!("world");
        }
        "#
        .unindent(),
    );

    cx.set_diff_base(&diff_base);
    executor.run_until_parked();

    cx.update_editor(|editor, cx| {
        //Wrap around the bottom of the buffer
        for _ in 0..3 {
            editor.go_to_next_hunk(&GoToHunk, cx);
        }
    });

    cx.assert_editor_state(
        &r#"
        ˇuse some::modified;


        fn main() {
            println!("hello there");

            println!("around the");
            println!("world");
        }
        "#
        .unindent(),
    );

    cx.update_editor(|editor, cx| {
        //Wrap around the top of the buffer
        for _ in 0..2 {
            editor.go_to_prev_hunk(&GoToPrevHunk, cx);
        }
    });

    cx.assert_editor_state(
        &r#"
        use some::modified;


        fn main() {
        ˇ    println!("hello there");

            println!("around the");
            println!("world");
        }
        "#
        .unindent(),
    );

    cx.update_editor(|editor, cx| {
        editor.go_to_prev_hunk(&GoToPrevHunk, cx);
    });

    cx.assert_editor_state(
        &r#"
        use some::modified;

        ˇ
        fn main() {
            println!("hello there");

            println!("around the");
            println!("world");
        }
        "#
        .unindent(),
    );

    cx.update_editor(|editor, cx| {
        for _ in 0..3 {
            editor.go_to_prev_hunk(&GoToPrevHunk, cx);
        }
    });

    cx.assert_editor_state(
        &r#"
        use some::modified;


        fn main() {
        ˇ    println!("hello there");

            println!("around the");
            println!("world");
        }
        "#
        .unindent(),
    );

    cx.update_editor(|editor, cx| {
        editor.fold(&Fold, cx);

        //Make sure that the fold only gets one hunk
        for _ in 0..4 {
            editor.go_to_next_hunk(&GoToHunk, cx);
        }
    });

    cx.assert_editor_state(
        &r#"
        ˇuse some::modified;


        fn main() {
            println!("hello there");

            println!("around the");
            println!("world");
        }
        "#
        .unindent(),
    );
}

#[test]
fn test_split_words() {
    fn split(text: &str) -> Vec<&str> {
        split_words(text).collect()
    }

    assert_eq!(split("HelloWorld"), &["Hello", "World"]);
    assert_eq!(split("hello_world"), &["hello_", "world"]);
    assert_eq!(split("_hello_world_"), &["_", "hello_", "world_"]);
    assert_eq!(split("Hello_World"), &["Hello_", "World"]);
    assert_eq!(split("helloWOrld"), &["hello", "WOrld"]);
    assert_eq!(split("helloworld"), &["helloworld"]);

    assert_eq!(split(":do_the_thing"), &[":", "do_", "the_", "thing"]);
}

#[gpui::test]
async fn test_move_to_enclosing_bracket(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorLspTestContext::new_typescript(Default::default(), cx).await;
    let mut assert = |before, after| {
        let _state_context = cx.set_state(before);
        cx.update_editor(|editor, cx| {
            editor.move_to_enclosing_bracket(&MoveToEnclosingBracket, cx)
        });
        cx.assert_editor_state(after);
    };

    // Outside bracket jumps to outside of matching bracket
    assert("console.logˇ(var);", "console.log(var)ˇ;");
    assert("console.log(var)ˇ;", "console.logˇ(var);");

    // Inside bracket jumps to inside of matching bracket
    assert("console.log(ˇvar);", "console.log(varˇ);");
    assert("console.log(varˇ);", "console.log(ˇvar);");

    // When outside a bracket and inside, favor jumping to the inside bracket
    assert(
        "console.log('foo', [1, 2, 3]ˇ);",
        "console.log(ˇ'foo', [1, 2, 3]);",
    );
    assert(
        "console.log(ˇ'foo', [1, 2, 3]);",
        "console.log('foo', [1, 2, 3]ˇ);",
    );

    // Bias forward if two options are equally likely
    assert(
        "let result = curried_fun()ˇ();",
        "let result = curried_fun()()ˇ;",
    );

    // If directly adjacent to a smaller pair but inside a larger (not adjacent), pick the smaller
    assert(
        indoc! {"
            function test() {
                console.log('test')ˇ
            }"},
        indoc! {"
            function test() {
                console.logˇ('test')
            }"},
    );
}

#[gpui::test]
async fn test_on_type_formatting_not_triggered(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(
        "/a",
        json!({
            "main.rs": "fn main() { let a = 5; }",
            "other.rs": "// Test file",
        }),
    )
    .await;
    let project = Project::test(fs, ["/a".as_ref()], cx).await;

    let language_registry = project.read_with(cx, |project, _| project.languages().clone());
    language_registry.add(Arc::new(Language::new(
        LanguageConfig {
            name: "Rust".into(),
            matcher: LanguageMatcher {
                path_suffixes: vec!["rs".to_string()],
                ..Default::default()
            },
            brackets: BracketPairConfig {
                pairs: vec![BracketPair {
                    start: "{".to_string(),
                    end: "}".to_string(),
                    close: true,
                    surround: true,
                    newline: true,
                }],
                disabled_scopes_by_bracket_ix: Vec::new(),
            },
            ..Default::default()
        },
        Some(tree_sitter_rust::LANGUAGE.into()),
    )));
    let mut fake_servers = language_registry.register_fake_lsp(
        "Rust",
        FakeLspAdapter {
            capabilities: lsp::ServerCapabilities {
                document_on_type_formatting_provider: Some(lsp::DocumentOnTypeFormattingOptions {
                    first_trigger_character: "{".to_string(),
                    more_trigger_character: None,
                }),
                ..Default::default()
            },
            ..Default::default()
        },
    );

    let workspace = cx.add_window(|cx| Workspace::test_new(project.clone(), cx));

    let cx = &mut VisualTestContext::from_window(*workspace, cx);

    let worktree_id = workspace
        .update(cx, |workspace, cx| {
            workspace.project().update(cx, |project, cx| {
                project.worktrees(cx).next().unwrap().read(cx).id()
            })
        })
        .unwrap();

    let buffer = project
        .update(cx, |project, cx| {
            project.open_local_buffer("/a/main.rs", cx)
        })
        .await
        .unwrap();
    let editor_handle = workspace
        .update(cx, |workspace, cx| {
            workspace.open_path((worktree_id, "main.rs"), None, true, cx)
        })
        .unwrap()
        .await
        .unwrap()
        .downcast::<Editor>()
        .unwrap();

    cx.executor().start_waiting();
    let fake_server = fake_servers.next().await.unwrap();

    fake_server.handle_request::<lsp::request::OnTypeFormatting, _, _>(|params, _| async move {
        assert_eq!(
            params.text_document_position.text_document.uri,
            lsp::Url::from_file_path("/a/main.rs").unwrap(),
        );
        assert_eq!(
            params.text_document_position.position,
            lsp::Position::new(0, 21),
        );

        Ok(Some(vec![lsp::TextEdit {
            new_text: "]".to_string(),
            range: lsp::Range::new(lsp::Position::new(0, 22), lsp::Position::new(0, 22)),
        }]))
    });

    editor_handle.update(cx, |editor, cx| {
        editor.focus(cx);
        editor.change_selections(None, cx, |s| {
            s.select_ranges([Point::new(0, 21)..Point::new(0, 20)])
        });
        editor.handle_input("{", cx);
    });

    cx.executor().run_until_parked();

    buffer.update(cx, |buffer, _| {
        assert_eq!(
            buffer.text(),
            "fn main() { let a = {5}; }",
            "No extra braces from on type formatting should appear in the buffer"
        )
    });
}

#[gpui::test]
async fn test_language_server_restart_due_to_settings_change(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(
        "/a",
        json!({
            "main.rs": "fn main() { let a = 5; }",
            "other.rs": "// Test file",
        }),
    )
    .await;

    let project = Project::test(fs, ["/a".as_ref()], cx).await;

    let server_restarts = Arc::new(AtomicUsize::new(0));
    let closure_restarts = Arc::clone(&server_restarts);
    let language_server_name = "test language server";
    let language_name: LanguageName = "Rust".into();

    let language_registry = project.read_with(cx, |project, _| project.languages().clone());
    language_registry.add(Arc::new(Language::new(
        LanguageConfig {
            name: language_name.clone(),
            matcher: LanguageMatcher {
                path_suffixes: vec!["rs".to_string()],
                ..Default::default()
            },
            ..Default::default()
        },
        Some(tree_sitter_rust::LANGUAGE.into()),
    )));
    let mut fake_servers = language_registry.register_fake_lsp(
        "Rust",
        FakeLspAdapter {
            name: language_server_name,
            initialization_options: Some(json!({
                "testOptionValue": true
            })),
            initializer: Some(Box::new(move |fake_server| {
                let task_restarts = Arc::clone(&closure_restarts);
                fake_server.handle_request::<lsp::request::Shutdown, _, _>(move |_, _| {
                    task_restarts.fetch_add(1, atomic::Ordering::Release);
                    futures::future::ready(Ok(()))
                });
            })),
            ..Default::default()
        },
    );

    let _window = cx.add_window(|cx| Workspace::test_new(project.clone(), cx));
    let _buffer = project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp("/a/main.rs", cx)
        })
        .await
        .unwrap();
    let _fake_server = fake_servers.next().await.unwrap();
    update_test_language_settings(cx, |language_settings| {
        language_settings.languages.insert(
            language_name.clone(),
            LanguageSettingsContent {
                tab_size: NonZeroU32::new(8),
                ..Default::default()
            },
        );
    });
    cx.executor().run_until_parked();
    assert_eq!(
        server_restarts.load(atomic::Ordering::Acquire),
        0,
        "Should not restart LSP server on an unrelated change"
    );

    update_test_project_settings(cx, |project_settings| {
        project_settings.lsp.insert(
            "Some other server name".into(),
            LspSettings {
                binary: None,
                settings: None,
                initialization_options: Some(json!({
                    "some other init value": false
                })),
            },
        );
    });
    cx.executor().run_until_parked();
    assert_eq!(
        server_restarts.load(atomic::Ordering::Acquire),
        0,
        "Should not restart LSP server on an unrelated LSP settings change"
    );

    update_test_project_settings(cx, |project_settings| {
        project_settings.lsp.insert(
            language_server_name.into(),
            LspSettings {
                binary: None,
                settings: None,
                initialization_options: Some(json!({
                    "anotherInitValue": false
                })),
            },
        );
    });
    cx.executor().run_until_parked();
    assert_eq!(
        server_restarts.load(atomic::Ordering::Acquire),
        1,
        "Should restart LSP server on a related LSP settings change"
    );

    update_test_project_settings(cx, |project_settings| {
        project_settings.lsp.insert(
            language_server_name.into(),
            LspSettings {
                binary: None,
                settings: None,
                initialization_options: Some(json!({
                    "anotherInitValue": false
                })),
            },
        );
    });
    cx.executor().run_until_parked();
    assert_eq!(
        server_restarts.load(atomic::Ordering::Acquire),
        1,
        "Should not restart LSP server on a related LSP settings change that is the same"
    );

    update_test_project_settings(cx, |project_settings| {
        project_settings.lsp.insert(
            language_server_name.into(),
            LspSettings {
                binary: None,
                settings: None,
                initialization_options: None,
            },
        );
    });
    cx.executor().run_until_parked();
    assert_eq!(
        server_restarts.load(atomic::Ordering::Acquire),
        2,
        "Should restart LSP server on another related LSP settings change"
    );
}

#[gpui::test]
async fn test_completions_with_additional_edits(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorLspTestContext::new_rust(
        lsp::ServerCapabilities {
            completion_provider: Some(lsp::CompletionOptions {
                trigger_characters: Some(vec![".".to_string()]),
                resolve_provider: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        },
        cx,
    )
    .await;

    cx.set_state(indoc! {"fn main() { let a = 2ˇ; }"});
    cx.simulate_keystroke(".");
    let completion_item = lsp::CompletionItem {
        label: "some".into(),
        kind: Some(lsp::CompletionItemKind::SNIPPET),
        detail: Some("Wrap the expression in an `Option::Some`".to_string()),
        documentation: Some(lsp::Documentation::MarkupContent(lsp::MarkupContent {
            kind: lsp::MarkupKind::Markdown,
            value: "```rust\nSome(2)\n```".to_string(),
        })),
        deprecated: Some(false),
        sort_text: Some("fffffff2".to_string()),
        filter_text: Some("some".to_string()),
        insert_text_format: Some(lsp::InsertTextFormat::SNIPPET),
        text_edit: Some(lsp::CompletionTextEdit::Edit(lsp::TextEdit {
            range: lsp::Range {
                start: lsp::Position {
                    line: 0,
                    character: 22,
                },
                end: lsp::Position {
                    line: 0,
                    character: 22,
                },
            },
            new_text: "Some(2)".to_string(),
        })),
        additional_text_edits: Some(vec![lsp::TextEdit {
            range: lsp::Range {
                start: lsp::Position {
                    line: 0,
                    character: 20,
                },
                end: lsp::Position {
                    line: 0,
                    character: 22,
                },
            },
            new_text: "".to_string(),
        }]),
        ..Default::default()
    };

    let closure_completion_item = completion_item.clone();
    let mut request = cx.handle_request::<lsp::request::Completion, _, _>(move |_, _, _| {
        let task_completion_item = closure_completion_item.clone();
        async move {
            Ok(Some(lsp::CompletionResponse::Array(vec![
                task_completion_item,
            ])))
        }
    });

    request.next().await;

    cx.condition(|editor, _| editor.context_menu_visible())
        .await;
    let apply_additional_edits = cx.update_editor(|editor, cx| {
        editor
            .confirm_completion(&ConfirmCompletion::default(), cx)
            .unwrap()
    });
    cx.assert_editor_state(indoc! {"fn main() { let a = 2.Some(2)ˇ; }"});

    cx.handle_request::<lsp::request::ResolveCompletionItem, _, _>(move |_, _, _| {
        let task_completion_item = completion_item.clone();
        async move { Ok(task_completion_item) }
    })
    .next()
    .await
    .unwrap();
    apply_additional_edits.await.unwrap();
    cx.assert_editor_state(indoc! {"fn main() { let a = Some(2)ˇ; }"});
}

#[gpui::test]
async fn test_completions_resolve_updates_labels_if_filter_text_matches(
    cx: &mut gpui::TestAppContext,
) {
    init_test(cx, |_| {});

    let mut cx = EditorLspTestContext::new_rust(
        lsp::ServerCapabilities {
            completion_provider: Some(lsp::CompletionOptions {
                trigger_characters: Some(vec![".".to_string()]),
                resolve_provider: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        },
        cx,
    )
    .await;

    cx.set_state(indoc! {"fn main() { let a = 2ˇ; }"});
    cx.simulate_keystroke(".");

    let item1 = lsp::CompletionItem {
        label: "id".to_string(),
        filter_text: Some("id".to_string()),
        detail: None,
        documentation: None,
        text_edit: Some(lsp::CompletionTextEdit::Edit(lsp::TextEdit {
            range: lsp::Range::new(lsp::Position::new(0, 22), lsp::Position::new(0, 22)),
            new_text: ".id".to_string(),
        })),
        ..lsp::CompletionItem::default()
    };

    let item2 = lsp::CompletionItem {
        label: "other".to_string(),
        filter_text: Some("other".to_string()),
        detail: None,
        documentation: None,
        text_edit: Some(lsp::CompletionTextEdit::Edit(lsp::TextEdit {
            range: lsp::Range::new(lsp::Position::new(0, 22), lsp::Position::new(0, 22)),
            new_text: ".other".to_string(),
        })),
        ..lsp::CompletionItem::default()
    };

    let item1 = item1.clone();
    cx.handle_request::<lsp::request::Completion, _, _>({
        let item1 = item1.clone();
        move |_, _, _| {
            let item1 = item1.clone();
            let item2 = item2.clone();
            async move { Ok(Some(lsp::CompletionResponse::Array(vec![item1, item2]))) }
        }
    })
    .next()
    .await;

    cx.condition(|editor, _| editor.context_menu_visible())
        .await;
    cx.update_editor(|editor, _| {
        let context_menu = editor.context_menu.borrow_mut();
        let context_menu = context_menu
            .as_ref()
            .expect("Should have the context menu deployed");
        match context_menu {
            CodeContextMenu::Completions(completions_menu) => {
                let completions = completions_menu.completions.borrow_mut();
                assert_eq!(
                    completions
                        .iter()
                        .map(|completion| &completion.label.text)
                        .collect::<Vec<_>>(),
                    vec!["id", "other"]
                )
            }
            CodeContextMenu::CodeActions(_) => panic!("Should show the completions menu"),
        }
    });

    cx.handle_request::<lsp::request::ResolveCompletionItem, _, _>({
        let item1 = item1.clone();
        move |_, item_to_resolve, _| {
            let item1 = item1.clone();
            async move {
                if item1 == item_to_resolve {
                    Ok(lsp::CompletionItem {
                        label: "method id()".to_string(),
                        filter_text: Some("id".to_string()),
                        detail: Some("Now resolved!".to_string()),
                        documentation: Some(lsp::Documentation::String("Docs".to_string())),
                        text_edit: Some(lsp::CompletionTextEdit::Edit(lsp::TextEdit {
                            range: lsp::Range::new(
                                lsp::Position::new(0, 22),
                                lsp::Position::new(0, 22),
                            ),
                            new_text: ".id".to_string(),
                        })),
                        ..lsp::CompletionItem::default()
                    })
                } else {
                    Ok(item_to_resolve)
                }
            }
        }
    })
    .next()
    .await
    .unwrap();
    cx.run_until_parked();

    cx.update_editor(|editor, cx| {
        editor.context_menu_next(&Default::default(), cx);
    });

    cx.update_editor(|editor, _| {
        let context_menu = editor.context_menu.borrow_mut();
        let context_menu = context_menu
            .as_ref()
            .expect("Should have the context menu deployed");
        match context_menu {
            CodeContextMenu::Completions(completions_menu) => {
                let completions = completions_menu.completions.borrow_mut();
                assert_eq!(
                    completions
                        .iter()
                        .map(|completion| &completion.label.text)
                        .collect::<Vec<_>>(),
                    vec!["method id()", "other"],
                    "Should update first completion label, but not second as the filter text did not match."
                );
            }
            CodeContextMenu::CodeActions(_) => panic!("Should show the completions menu"),
        }
    });
}

#[gpui::test]
async fn test_completions_resolve_happens_once(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorLspTestContext::new_rust(
        lsp::ServerCapabilities {
            completion_provider: Some(lsp::CompletionOptions {
                trigger_characters: Some(vec![".".to_string()]),
                resolve_provider: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        },
        cx,
    )
    .await;

    cx.set_state(indoc! {"fn main() { let a = 2ˇ; }"});
    cx.simulate_keystroke(".");

    let unresolved_item_1 = lsp::CompletionItem {
        label: "id".to_string(),
        filter_text: Some("id".to_string()),
        detail: None,
        documentation: None,
        text_edit: Some(lsp::CompletionTextEdit::Edit(lsp::TextEdit {
            range: lsp::Range::new(lsp::Position::new(0, 22), lsp::Position::new(0, 22)),
            new_text: ".id".to_string(),
        })),
        ..lsp::CompletionItem::default()
    };
    let resolved_item_1 = lsp::CompletionItem {
        additional_text_edits: Some(vec![lsp::TextEdit {
            range: lsp::Range::new(lsp::Position::new(0, 20), lsp::Position::new(0, 22)),
            new_text: "!!".to_string(),
        }]),
        ..unresolved_item_1.clone()
    };
    let unresolved_item_2 = lsp::CompletionItem {
        label: "other".to_string(),
        filter_text: Some("other".to_string()),
        detail: None,
        documentation: None,
        text_edit: Some(lsp::CompletionTextEdit::Edit(lsp::TextEdit {
            range: lsp::Range::new(lsp::Position::new(0, 22), lsp::Position::new(0, 22)),
            new_text: ".other".to_string(),
        })),
        ..lsp::CompletionItem::default()
    };
    let resolved_item_2 = lsp::CompletionItem {
        additional_text_edits: Some(vec![lsp::TextEdit {
            range: lsp::Range::new(lsp::Position::new(0, 20), lsp::Position::new(0, 22)),
            new_text: "??".to_string(),
        }]),
        ..unresolved_item_2.clone()
    };

    let resolve_requests_1 = Arc::new(AtomicUsize::new(0));
    let resolve_requests_2 = Arc::new(AtomicUsize::new(0));
    cx.lsp
        .server
        .on_request::<lsp::request::ResolveCompletionItem, _, _>({
            let unresolved_item_1 = unresolved_item_1.clone();
            let resolved_item_1 = resolved_item_1.clone();
            let unresolved_item_2 = unresolved_item_2.clone();
            let resolved_item_2 = resolved_item_2.clone();
            let resolve_requests_1 = resolve_requests_1.clone();
            let resolve_requests_2 = resolve_requests_2.clone();
            move |unresolved_request, _| {
                let unresolved_item_1 = unresolved_item_1.clone();
                let resolved_item_1 = resolved_item_1.clone();
                let unresolved_item_2 = unresolved_item_2.clone();
                let resolved_item_2 = resolved_item_2.clone();
                let resolve_requests_1 = resolve_requests_1.clone();
                let resolve_requests_2 = resolve_requests_2.clone();
                async move {
                    if unresolved_request == unresolved_item_1 {
                        resolve_requests_1.fetch_add(1, atomic::Ordering::Release);
                        Ok(resolved_item_1.clone())
                    } else if unresolved_request == unresolved_item_2 {
                        resolve_requests_2.fetch_add(1, atomic::Ordering::Release);
                        Ok(resolved_item_2.clone())
                    } else {
                        panic!("Unexpected completion item {unresolved_request:?}")
                    }
                }
            }
        })
        .detach();

    cx.handle_request::<lsp::request::Completion, _, _>(move |_, _, _| {
        let unresolved_item_1 = unresolved_item_1.clone();
        let unresolved_item_2 = unresolved_item_2.clone();
        async move {
            Ok(Some(lsp::CompletionResponse::Array(vec![
                unresolved_item_1,
                unresolved_item_2,
            ])))
        }
    })
    .next()
    .await;

    cx.condition(|editor, _| editor.context_menu_visible())
        .await;
    cx.update_editor(|editor, _| {
        let context_menu = editor.context_menu.borrow_mut();
        let context_menu = context_menu
            .as_ref()
            .expect("Should have the context menu deployed");
        match context_menu {
            CodeContextMenu::Completions(completions_menu) => {
                let completions = completions_menu.completions.borrow_mut();
                assert_eq!(
                    completions
                        .iter()
                        .map(|completion| &completion.label.text)
                        .collect::<Vec<_>>(),
                    vec!["id", "other"]
                )
            }
            CodeContextMenu::CodeActions(_) => panic!("Should show the completions menu"),
        }
    });
    cx.run_until_parked();

    cx.update_editor(|editor, cx| {
        editor.context_menu_next(&ContextMenuNext, cx);
    });
    cx.run_until_parked();
    cx.update_editor(|editor, cx| {
        editor.context_menu_prev(&ContextMenuPrev, cx);
    });
    cx.run_until_parked();
    cx.update_editor(|editor, cx| {
        editor.context_menu_next(&ContextMenuNext, cx);
    });
    cx.run_until_parked();
    cx.update_editor(|editor, cx| {
        editor
            .compose_completion(&ComposeCompletion::default(), cx)
            .expect("No task returned")
    })
    .await
    .expect("Completion failed");
    cx.run_until_parked();

    cx.update_editor(|editor, cx| {
        assert_eq!(
            resolve_requests_1.load(atomic::Ordering::Acquire),
            1,
            "Should always resolve once despite multiple selections"
        );
        assert_eq!(
            resolve_requests_2.load(atomic::Ordering::Acquire),
            1,
            "Should always resolve once after multiple selections and applying the completion"
        );
        assert_eq!(
            editor.text(cx),
            "fn main() { let a = ??.other; }",
            "Should use resolved data when applying the completion"
        );
    });
}

#[gpui::test]
async fn test_completions_default_resolve_data_handling(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorLspTestContext::new_rust(
        lsp::ServerCapabilities {
            completion_provider: Some(lsp::CompletionOptions {
                trigger_characters: Some(vec![".".to_string()]),
                resolve_provider: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        },
        cx,
    )
    .await;

    cx.set_state(indoc! {"fn main() { let a = 2ˇ; }"});
    cx.simulate_keystroke(".");

    let default_commit_characters = vec!["?".to_string()];
    let default_data = json!({ "very": "special"});
    let default_insert_text_format = lsp::InsertTextFormat::SNIPPET;
    let default_insert_text_mode = lsp::InsertTextMode::AS_IS;
    let default_edit_range = lsp::Range {
        start: lsp::Position {
            line: 0,
            character: 5,
        },
        end: lsp::Position {
            line: 0,
            character: 5,
        },
    };

    let resolve_requests_number = Arc::new(AtomicUsize::new(0));
    let expect_first_item = Arc::new(AtomicBool::new(true));
    cx.lsp
        .server
        .on_request::<lsp::request::ResolveCompletionItem, _, _>({
            let closure_default_data = default_data.clone();
            let closure_resolve_requests_number = resolve_requests_number.clone();
            let closure_expect_first_item = expect_first_item.clone();
            let closure_default_commit_characters = default_commit_characters.clone();
            move |item_to_resolve, _| {
                closure_resolve_requests_number.fetch_add(1, atomic::Ordering::Release);
                let default_data = closure_default_data.clone();
                let default_commit_characters = closure_default_commit_characters.clone();
                let expect_first_item = closure_expect_first_item.clone();
                async move {
                    if expect_first_item.load(atomic::Ordering::Acquire) {
                        assert_eq!(
                            item_to_resolve.label, "Some(2)",
                            "Should have selected the first item"
                        );
                        assert_eq!(
                            item_to_resolve.data,
                            Some(json!({ "very": "special"})),
                            "First item should bring its own data for resolving"
                        );
                        assert_eq!(
                            item_to_resolve.commit_characters,
                            Some(default_commit_characters),
                            "First item had no own commit characters and should inherit the default ones"
                        );
                        assert!(
                            matches!(
                                item_to_resolve.text_edit,
                                Some(lsp::CompletionTextEdit::InsertAndReplace { .. })
                            ),
                            "First item should bring its own edit range for resolving"
                        );
                        assert_eq!(
                            item_to_resolve.insert_text_format,
                            Some(default_insert_text_format),
                            "First item had no own insert text format and should inherit the default one"
                        );
                        assert_eq!(
                            item_to_resolve.insert_text_mode,
                            Some(lsp::InsertTextMode::ADJUST_INDENTATION),
                            "First item should bring its own insert text mode for resolving"
                        );
                        Ok(item_to_resolve)
                    } else {
                        assert_eq!(
                            item_to_resolve.label, "vec![2]",
                            "Should have selected the last item"
                        );
                        assert_eq!(
                            item_to_resolve.data,
                            Some(default_data),
                            "Last item has no own resolve data and should inherit the default one"
                        );
                        assert_eq!(
                            item_to_resolve.commit_characters,
                            Some(default_commit_characters),
                            "Last item had no own commit characters and should inherit the default ones"
                        );
                        assert_eq!(
                            item_to_resolve.text_edit,
                            Some(lsp::CompletionTextEdit::Edit(lsp::TextEdit {
                                range: default_edit_range,
                                new_text: "vec![2]".to_string()
                            })),
                            "Last item had no own edit range and should inherit the default one"
                        );
                        assert_eq!(
                            item_to_resolve.insert_text_format,
                            Some(lsp::InsertTextFormat::PLAIN_TEXT),
                            "Last item should bring its own insert text format for resolving"
                        );
                        assert_eq!(
                            item_to_resolve.insert_text_mode,
                            Some(default_insert_text_mode),
                            "Last item had no own insert text mode and should inherit the default one"
                        );

                        Ok(item_to_resolve)
                    }
                }
            }
        }).detach();

    let completion_data = default_data.clone();
    let completion_characters = default_commit_characters.clone();
    cx.handle_request::<lsp::request::Completion, _, _>(move |_, _, _| {
        let default_data = completion_data.clone();
        let default_commit_characters = completion_characters.clone();
        async move {
            Ok(Some(lsp::CompletionResponse::List(lsp::CompletionList {
                items: vec![
                    lsp::CompletionItem {
                        label: "Some(2)".into(),
                        insert_text: Some("Some(2)".into()),
                        data: Some(json!({ "very": "special"})),
                        insert_text_mode: Some(lsp::InsertTextMode::ADJUST_INDENTATION),
                        text_edit: Some(lsp::CompletionTextEdit::InsertAndReplace(
                            lsp::InsertReplaceEdit {
                                new_text: "Some(2)".to_string(),
                                insert: lsp::Range::default(),
                                replace: lsp::Range::default(),
                            },
                        )),
                        ..lsp::CompletionItem::default()
                    },
                    lsp::CompletionItem {
                        label: "vec![2]".into(),
                        insert_text: Some("vec![2]".into()),
                        insert_text_format: Some(lsp::InsertTextFormat::PLAIN_TEXT),
                        ..lsp::CompletionItem::default()
                    },
                ],
                item_defaults: Some(lsp::CompletionListItemDefaults {
                    data: Some(default_data.clone()),
                    commit_characters: Some(default_commit_characters.clone()),
                    edit_range: Some(lsp::CompletionListItemDefaultsEditRange::Range(
                        default_edit_range,
                    )),
                    insert_text_format: Some(default_insert_text_format),
                    insert_text_mode: Some(default_insert_text_mode),
                }),
                ..lsp::CompletionList::default()
            })))
        }
    })
    .next()
    .await;

    cx.condition(|editor, _| editor.context_menu_visible())
        .await;
    cx.run_until_parked();
    cx.update_editor(|editor, _| {
        let menu = editor.context_menu.borrow_mut();
        match menu.as_ref().expect("should have the completions menu") {
            CodeContextMenu::Completions(completions_menu) => {
                assert_eq!(
                    completion_menu_entries(&completions_menu.entries),
                    vec!["Some(2)", "vec![2]"]
                );
            }
            CodeContextMenu::CodeActions(_) => panic!("Expected to have the completions menu"),
        }
    });
    assert_eq!(
        resolve_requests_number.load(atomic::Ordering::Acquire),
        1,
        "While there are 2 items in the completion list, only 1 resolve request should have been sent, for the selected item"
    );

    cx.update_editor(|editor, cx| {
        editor.context_menu_first(&ContextMenuFirst, cx);
    });
    cx.run_until_parked();
    assert_eq!(
        resolve_requests_number.load(atomic::Ordering::Acquire),
        1,
        "After re-selecting the first item, no new resolve requests should be sent"
    );

    expect_first_item.store(false, atomic::Ordering::Release);
    cx.update_editor(|editor, cx| {
        editor.context_menu_last(&ContextMenuLast, cx);
    });
    cx.run_until_parked();
    assert_eq!(
        resolve_requests_number.load(atomic::Ordering::Acquire),
        2,
        "After selecting the other item, another resolve request should have been sent"
    );
}

#[gpui::test]
async fn test_completions_in_languages_with_extra_word_characters(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorLspTestContext::new(
        Language::new(
            LanguageConfig {
                matcher: LanguageMatcher {
                    path_suffixes: vec!["jsx".into()],
                    ..Default::default()
                },
                overrides: [(
                    "element".into(),
                    LanguageConfigOverride {
                        word_characters: Override::Set(['-'].into_iter().collect()),
                        ..Default::default()
                    },
                )]
                .into_iter()
                .collect(),
                ..Default::default()
            },
            Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
        )
        .with_override_query("(jsx_self_closing_element) @element")
        .unwrap(),
        lsp::ServerCapabilities {
            completion_provider: Some(lsp::CompletionOptions {
                trigger_characters: Some(vec![":".to_string()]),
                ..Default::default()
            }),
            ..Default::default()
        },
        cx,
    )
    .await;

    cx.lsp
        .handle_request::<lsp::request::Completion, _, _>(move |_, _| async move {
            Ok(Some(lsp::CompletionResponse::Array(vec![
                lsp::CompletionItem {
                    label: "bg-blue".into(),
                    ..Default::default()
                },
                lsp::CompletionItem {
                    label: "bg-red".into(),
                    ..Default::default()
                },
                lsp::CompletionItem {
                    label: "bg-yellow".into(),
                    ..Default::default()
                },
            ])))
        });

    cx.set_state(r#"<p class="bgˇ" />"#);

    // Trigger completion when typing a dash, because the dash is an extra
    // word character in the 'element' scope, which contains the cursor.
    cx.simulate_keystroke("-");
    cx.executor().run_until_parked();
    cx.update_editor(|editor, _| {
        if let Some(CodeContextMenu::Completions(menu)) = editor.context_menu.borrow_mut().as_ref()
        {
            assert_eq!(
                completion_menu_entries(&menu.entries),
                &["bg-red", "bg-blue", "bg-yellow"]
            );
        } else {
            panic!("expected completion menu to be open");
        }
    });

    cx.simulate_keystroke("l");
    cx.executor().run_until_parked();
    cx.update_editor(|editor, _| {
        if let Some(CodeContextMenu::Completions(menu)) = editor.context_menu.borrow_mut().as_ref()
        {
            assert_eq!(
                completion_menu_entries(&menu.entries),
                &["bg-blue", "bg-yellow"]
            );
        } else {
            panic!("expected completion menu to be open");
        }
    });

    // When filtering completions, consider the character after the '-' to
    // be the start of a subword.
    cx.set_state(r#"<p class="yelˇ" />"#);
    cx.simulate_keystroke("l");
    cx.executor().run_until_parked();
    cx.update_editor(|editor, _| {
        if let Some(CodeContextMenu::Completions(menu)) = editor.context_menu.borrow_mut().as_ref()
        {
            assert_eq!(completion_menu_entries(&menu.entries), &["bg-yellow"]);
        } else {
            panic!("expected completion menu to be open");
        }
    });
}

fn completion_menu_entries(entries: &[CompletionEntry]) -> Vec<&str> {
    entries
        .iter()
        .flat_map(|e| match e {
            CompletionEntry::Match(mat) => Some(mat.string.as_str()),
            _ => None,
        })
        .collect()
}

#[gpui::test]
async fn test_document_format_with_prettier(cx: &mut gpui::TestAppContext) {
    init_test(cx, |settings| {
        settings.defaults.formatter = Some(language_settings::SelectedFormatter::List(
            FormatterList(vec![Formatter::Prettier].into()),
        ))
    });

    let fs = FakeFs::new(cx.executor());
    fs.insert_file("/file.ts", Default::default()).await;

    let project = Project::test(fs, ["/file.ts".as_ref()], cx).await;
    let language_registry = project.read_with(cx, |project, _| project.languages().clone());

    language_registry.add(Arc::new(Language::new(
        LanguageConfig {
            name: "TypeScript".into(),
            matcher: LanguageMatcher {
                path_suffixes: vec!["ts".to_string()],
                ..Default::default()
            },
            ..Default::default()
        },
        Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
    )));
    update_test_language_settings(cx, |settings| {
        settings.defaults.prettier = Some(PrettierSettings {
            allowed: true,
            ..PrettierSettings::default()
        });
    });

    let test_plugin = "test_plugin";
    let _ = language_registry.register_fake_lsp(
        "TypeScript",
        FakeLspAdapter {
            prettier_plugins: vec![test_plugin],
            ..Default::default()
        },
    );

    let prettier_format_suffix = project::TEST_PRETTIER_FORMAT_SUFFIX;
    let buffer = project
        .update(cx, |project, cx| project.open_local_buffer("/file.ts", cx))
        .await
        .unwrap();

    let buffer_text = "one\ntwo\nthree\n";
    let buffer = cx.new_model(|cx| MultiBuffer::singleton(buffer, cx));
    let (editor, cx) = cx.add_window_view(|cx| build_editor(buffer, cx));
    editor.update(cx, |editor, cx| editor.set_text(buffer_text, cx));

    editor
        .update(cx, |editor, cx| {
            editor.perform_format(
                project.clone(),
                FormatTrigger::Manual,
                FormatTarget::Buffer,
                cx,
            )
        })
        .unwrap()
        .await;
    assert_eq!(
        editor.update(cx, |editor, cx| editor.text(cx)),
        buffer_text.to_string() + prettier_format_suffix,
        "Test prettier formatting was not applied to the original buffer text",
    );

    update_test_language_settings(cx, |settings| {
        settings.defaults.formatter = Some(language_settings::SelectedFormatter::Auto)
    });
    let format = editor.update(cx, |editor, cx| {
        editor.perform_format(
            project.clone(),
            FormatTrigger::Manual,
            FormatTarget::Buffer,
            cx,
        )
    });
    format.await.unwrap();
    assert_eq!(
        editor.update(cx, |editor, cx| editor.text(cx)),
        buffer_text.to_string() + prettier_format_suffix + "\n" + prettier_format_suffix,
        "Autoformatting (via test prettier) was not applied to the original buffer text",
    );
}

#[gpui::test]
async fn test_addition_reverts(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});
    let mut cx = EditorLspTestContext::new_rust(lsp::ServerCapabilities::default(), cx).await;
    let base_text = indoc! {r#"
        struct Row;
        struct Row1;
        struct Row2;

        struct Row4;
        struct Row5;
        struct Row6;

        struct Row8;
        struct Row9;
        struct Row10;"#};

    // When addition hunks are not adjacent to carets, no hunk revert is performed
    assert_hunk_revert(
        indoc! {r#"struct Row;
                   struct Row1;
                   struct Row1.1;
                   struct Row1.2;
                   struct Row2;ˇ

                   struct Row4;
                   struct Row5;
                   struct Row6;

                   struct Row8;
                   ˇstruct Row9;
                   struct Row9.1;
                   struct Row9.2;
                   struct Row9.3;
                   struct Row10;"#},
        vec![DiffHunkStatus::Added, DiffHunkStatus::Added],
        indoc! {r#"struct Row;
                   struct Row1;
                   struct Row1.1;
                   struct Row1.2;
                   struct Row2;ˇ

                   struct Row4;
                   struct Row5;
                   struct Row6;

                   struct Row8;
                   ˇstruct Row9;
                   struct Row9.1;
                   struct Row9.2;
                   struct Row9.3;
                   struct Row10;"#},
        base_text,
        &mut cx,
    );
    // Same for selections
    assert_hunk_revert(
        indoc! {r#"struct Row;
                   struct Row1;
                   struct Row2;
                   struct Row2.1;
                   struct Row2.2;
                   «ˇ
                   struct Row4;
                   struct» Row5;
                   «struct Row6;
                   ˇ»
                   struct Row9.1;
                   struct Row9.2;
                   struct Row9.3;
                   struct Row8;
                   struct Row9;
                   struct Row10;"#},
        vec![DiffHunkStatus::Added, DiffHunkStatus::Added],
        indoc! {r#"struct Row;
                   struct Row1;
                   struct Row2;
                   struct Row2.1;
                   struct Row2.2;
                   «ˇ
                   struct Row4;
                   struct» Row5;
                   «struct Row6;
                   ˇ»
                   struct Row9.1;
                   struct Row9.2;
                   struct Row9.3;
                   struct Row8;
                   struct Row9;
                   struct Row10;"#},
        base_text,
        &mut cx,
    );

    // When carets and selections intersect the addition hunks, those are reverted.
    // Adjacent carets got merged.
    assert_hunk_revert(
        indoc! {r#"struct Row;
                   ˇ// something on the top
                   struct Row1;
                   struct Row2;
                   struct Roˇw3.1;
                   struct Row2.2;
                   struct Row2.3;ˇ

                   struct Row4;
                   struct ˇRow5.1;
                   struct Row5.2;
                   struct «Rowˇ»5.3;
                   struct Row5;
                   struct Row6;
                   ˇ
                   struct Row9.1;
                   struct «Rowˇ»9.2;
                   struct «ˇRow»9.3;
                   struct Row8;
                   struct Row9;
                   «ˇ// something on bottom»
                   struct Row10;"#},
        vec![
            DiffHunkStatus::Added,
            DiffHunkStatus::Added,
            DiffHunkStatus::Added,
            DiffHunkStatus::Added,
            DiffHunkStatus::Added,
        ],
        indoc! {r#"struct Row;
                   ˇstruct Row1;
                   struct Row2;
                   ˇ
                   struct Row4;
                   ˇstruct Row5;
                   struct Row6;
                   ˇ
                   ˇstruct Row8;
                   struct Row9;
                   ˇstruct Row10;"#},
        base_text,
        &mut cx,
    );
}

#[gpui::test]
async fn test_modification_reverts(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});
    let mut cx = EditorLspTestContext::new_rust(lsp::ServerCapabilities::default(), cx).await;
    let base_text = indoc! {r#"
        struct Row;
        struct Row1;
        struct Row2;

        struct Row4;
        struct Row5;
        struct Row6;

        struct Row8;
        struct Row9;
        struct Row10;"#};

    // Modification hunks behave the same as the addition ones.
    assert_hunk_revert(
        indoc! {r#"struct Row;
                   struct Row1;
                   struct Row33;
                   ˇ
                   struct Row4;
                   struct Row5;
                   struct Row6;
                   ˇ
                   struct Row99;
                   struct Row9;
                   struct Row10;"#},
        vec![DiffHunkStatus::Modified, DiffHunkStatus::Modified],
        indoc! {r#"struct Row;
                   struct Row1;
                   struct Row33;
                   ˇ
                   struct Row4;
                   struct Row5;
                   struct Row6;
                   ˇ
                   struct Row99;
                   struct Row9;
                   struct Row10;"#},
        base_text,
        &mut cx,
    );
    assert_hunk_revert(
        indoc! {r#"struct Row;
                   struct Row1;
                   struct Row33;
                   «ˇ
                   struct Row4;
                   struct» Row5;
                   «struct Row6;
                   ˇ»
                   struct Row99;
                   struct Row9;
                   struct Row10;"#},
        vec![DiffHunkStatus::Modified, DiffHunkStatus::Modified],
        indoc! {r#"struct Row;
                   struct Row1;
                   struct Row33;
                   «ˇ
                   struct Row4;
                   struct» Row5;
                   «struct Row6;
                   ˇ»
                   struct Row99;
                   struct Row9;
                   struct Row10;"#},
        base_text,
        &mut cx,
    );

    assert_hunk_revert(
        indoc! {r#"ˇstruct Row1.1;
                   struct Row1;
                   «ˇstr»uct Row22;

                   struct ˇRow44;
                   struct Row5;
                   struct «Rˇ»ow66;ˇ

                   «struˇ»ct Row88;
                   struct Row9;
                   struct Row1011;ˇ"#},
        vec![
            DiffHunkStatus::Modified,
            DiffHunkStatus::Modified,
            DiffHunkStatus::Modified,
            DiffHunkStatus::Modified,
            DiffHunkStatus::Modified,
            DiffHunkStatus::Modified,
        ],
        indoc! {r#"struct Row;
                   ˇstruct Row1;
                   struct Row2;
                   ˇ
                   struct Row4;
                   ˇstruct Row5;
                   struct Row6;
                   ˇ
                   struct Row8;
                   ˇstruct Row9;
                   struct Row10;ˇ"#},
        base_text,
        &mut cx,
    );
}

#[gpui::test]
async fn test_deletion_reverts(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});
    let mut cx = EditorLspTestContext::new_rust(lsp::ServerCapabilities::default(), cx).await;
    let base_text = indoc! {r#"struct Row;
struct Row1;
struct Row2;

struct Row4;
struct Row5;
struct Row6;

struct Row8;
struct Row9;
struct Row10;"#};

    // Deletion hunks trigger with carets on adjacent rows, so carets and selections have to stay farther to avoid the revert
    assert_hunk_revert(
        indoc! {r#"struct Row;
                   struct Row2;

                   ˇstruct Row4;
                   struct Row5;
                   struct Row6;
                   ˇ
                   struct Row8;
                   struct Row10;"#},
        vec![DiffHunkStatus::Removed, DiffHunkStatus::Removed],
        indoc! {r#"struct Row;
                   struct Row2;

                   ˇstruct Row4;
                   struct Row5;
                   struct Row6;
                   ˇ
                   struct Row8;
                   struct Row10;"#},
        base_text,
        &mut cx,
    );
    assert_hunk_revert(
        indoc! {r#"struct Row;
                   struct Row2;

                   «ˇstruct Row4;
                   struct» Row5;
                   «struct Row6;
                   ˇ»
                   struct Row8;
                   struct Row10;"#},
        vec![DiffHunkStatus::Removed, DiffHunkStatus::Removed],
        indoc! {r#"struct Row;
                   struct Row2;

                   «ˇstruct Row4;
                   struct» Row5;
                   «struct Row6;
                   ˇ»
                   struct Row8;
                   struct Row10;"#},
        base_text,
        &mut cx,
    );

    // Deletion hunks are ephemeral, so it's impossible to place the caret into them — Zed triggers reverts for lines, adjacent to carets and selections.
    assert_hunk_revert(
        indoc! {r#"struct Row;
                   ˇstruct Row2;

                   struct Row4;
                   struct Row5;
                   struct Row6;

                   struct Row8;ˇ
                   struct Row10;"#},
        vec![DiffHunkStatus::Removed, DiffHunkStatus::Removed],
        indoc! {r#"struct Row;
                   struct Row1;
                   ˇstruct Row2;

                   struct Row4;
                   struct Row5;
                   struct Row6;

                   struct Row8;ˇ
                   struct Row9;
                   struct Row10;"#},
        base_text,
        &mut cx,
    );
    assert_hunk_revert(
        indoc! {r#"struct Row;
                   struct Row2«ˇ;
                   struct Row4;
                   struct» Row5;
                   «struct Row6;

                   struct Row8;ˇ»
                   struct Row10;"#},
        vec![
            DiffHunkStatus::Removed,
            DiffHunkStatus::Removed,
            DiffHunkStatus::Removed,
        ],
        indoc! {r#"struct Row;
                   struct Row1;
                   struct Row2«ˇ;

                   struct Row4;
                   struct» Row5;
                   «struct Row6;

                   struct Row8;ˇ»
                   struct Row9;
                   struct Row10;"#},
        base_text,
        &mut cx,
    );
}

#[gpui::test]
async fn test_multibuffer_reverts(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let base_text_1 = "aaaa\nbbbb\ncccc\ndddd\neeee\nffff\ngggg\nhhhh\niiii\njjjj";
    let base_text_2 = "llll\nmmmm\nnnnn\noooo\npppp\nqqqq\nrrrr\nssss\ntttt\nuuuu";
    let base_text_3 =
        "vvvv\nwwww\nxxxx\nyyyy\nzzzz\n{{{{\n||||\n}}}}\n~~~~\n\u{7f}\u{7f}\u{7f}\u{7f}";

    let text_1 = edit_first_char_of_every_line(base_text_1);
    let text_2 = edit_first_char_of_every_line(base_text_2);
    let text_3 = edit_first_char_of_every_line(base_text_3);

    let buffer_1 = cx.new_model(|cx| Buffer::local(text_1.clone(), cx));
    let buffer_2 = cx.new_model(|cx| Buffer::local(text_2.clone(), cx));
    let buffer_3 = cx.new_model(|cx| Buffer::local(text_3.clone(), cx));

    let multibuffer = cx.new_model(|cx| {
        let mut multibuffer = MultiBuffer::new(ReadWrite);
        multibuffer.push_excerpts(
            buffer_1.clone(),
            [
                ExcerptRange {
                    context: Point::new(0, 0)..Point::new(3, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(5, 0)..Point::new(7, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(9, 0)..Point::new(10, 4),
                    primary: None,
                },
            ],
            cx,
        );
        multibuffer.push_excerpts(
            buffer_2.clone(),
            [
                ExcerptRange {
                    context: Point::new(0, 0)..Point::new(3, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(5, 0)..Point::new(7, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(9, 0)..Point::new(10, 4),
                    primary: None,
                },
            ],
            cx,
        );
        multibuffer.push_excerpts(
            buffer_3.clone(),
            [
                ExcerptRange {
                    context: Point::new(0, 0)..Point::new(3, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(5, 0)..Point::new(7, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(9, 0)..Point::new(10, 4),
                    primary: None,
                },
            ],
            cx,
        );
        multibuffer
    });

    let (editor, cx) = cx.add_window_view(|cx| build_editor(multibuffer, cx));
    editor.update(cx, |editor, cx| {
        for (buffer, diff_base) in [
            (buffer_1.clone(), base_text_1),
            (buffer_2.clone(), base_text_2),
            (buffer_3.clone(), base_text_3),
        ] {
            let change_set = cx.new_model(|cx| {
                BufferChangeSet::new_with_base_text(
                    diff_base.to_string(),
                    buffer.read(cx).text_snapshot(),
                    cx,
                )
            });
            editor.diff_map.add_change_set(change_set, cx)
        }
    });
    cx.executor().run_until_parked();

    editor.update(cx, |editor, cx| {
        assert_eq!(editor.text(cx), "Xaaa\nXbbb\nXccc\n\nXfff\nXggg\n\nXjjj\nXlll\nXmmm\nXnnn\n\nXqqq\nXrrr\n\nXuuu\nXvvv\nXwww\nXxxx\n\nX{{{\nX|||\n\nX\u{7f}\u{7f}\u{7f}");
        editor.select_all(&SelectAll, cx);
        editor.revert_selected_hunks(&RevertSelectedHunks, cx);
    });
    cx.executor().run_until_parked();

    // When all ranges are selected, all buffer hunks are reverted.
    editor.update(cx, |editor, cx| {
        assert_eq!(editor.text(cx), "aaaa\nbbbb\ncccc\ndddd\neeee\nffff\ngggg\nhhhh\niiii\njjjj\n\n\nllll\nmmmm\nnnnn\noooo\npppp\nqqqq\nrrrr\nssss\ntttt\nuuuu\n\n\nvvvv\nwwww\nxxxx\nyyyy\nzzzz\n{{{{\n||||\n}}}}\n~~~~\n\u{7f}\u{7f}\u{7f}\u{7f}\n\n");
    });
    buffer_1.update(cx, |buffer, _| {
        assert_eq!(buffer.text(), base_text_1);
    });
    buffer_2.update(cx, |buffer, _| {
        assert_eq!(buffer.text(), base_text_2);
    });
    buffer_3.update(cx, |buffer, _| {
        assert_eq!(buffer.text(), base_text_3);
    });

    editor.update(cx, |editor, cx| {
        editor.undo(&Default::default(), cx);
    });

    editor.update(cx, |editor, cx| {
        editor.change_selections(None, cx, |s| {
            s.select_ranges(Some(Point::new(0, 0)..Point::new(6, 0)));
        });
        editor.revert_selected_hunks(&RevertSelectedHunks, cx);
    });

    // Now, when all ranges selected belong to buffer_1, the revert should succeed,
    // but not affect buffer_2 and its related excerpts.
    editor.update(cx, |editor, cx| {
        assert_eq!(
            editor.text(cx),
            "aaaa\nbbbb\ncccc\ndddd\neeee\nffff\ngggg\nhhhh\niiii\njjjj\n\n\nXlll\nXmmm\nXnnn\n\nXqqq\nXrrr\n\nXuuu\nXvvv\nXwww\nXxxx\n\nX{{{\nX|||\n\nX\u{7f}\u{7f}\u{7f}"
        );
    });
    buffer_1.update(cx, |buffer, _| {
        assert_eq!(buffer.text(), base_text_1);
    });
    buffer_2.update(cx, |buffer, _| {
        assert_eq!(
            buffer.text(),
            "Xlll\nXmmm\nXnnn\nXooo\nXppp\nXqqq\nXrrr\nXsss\nXttt\nXuuu"
        );
    });
    buffer_3.update(cx, |buffer, _| {
        assert_eq!(
            buffer.text(),
            "Xvvv\nXwww\nXxxx\nXyyy\nXzzz\nX{{{\nX|||\nX}}}\nX~~~\nX\u{7f}\u{7f}\u{7f}"
        );
    });

    fn edit_first_char_of_every_line(text: &str) -> String {
        text.split('\n')
            .map(|line| format!("X{}", &line[1..]))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[gpui::test]
async fn test_mutlibuffer_in_navigation_history(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let cols = 4;
    let rows = 10;
    let sample_text_1 = sample_text(rows, cols, 'a');
    assert_eq!(
        sample_text_1,
        "aaaa\nbbbb\ncccc\ndddd\neeee\nffff\ngggg\nhhhh\niiii\njjjj"
    );
    let sample_text_2 = sample_text(rows, cols, 'l');
    assert_eq!(
        sample_text_2,
        "llll\nmmmm\nnnnn\noooo\npppp\nqqqq\nrrrr\nssss\ntttt\nuuuu"
    );
    let sample_text_3 = sample_text(rows, cols, 'v');
    assert_eq!(
        sample_text_3,
        "vvvv\nwwww\nxxxx\nyyyy\nzzzz\n{{{{\n||||\n}}}}\n~~~~\n\u{7f}\u{7f}\u{7f}\u{7f}"
    );

    let buffer_1 = cx.new_model(|cx| Buffer::local(sample_text_1.clone(), cx));
    let buffer_2 = cx.new_model(|cx| Buffer::local(sample_text_2.clone(), cx));
    let buffer_3 = cx.new_model(|cx| Buffer::local(sample_text_3.clone(), cx));

    let multi_buffer = cx.new_model(|cx| {
        let mut multibuffer = MultiBuffer::new(ReadWrite);
        multibuffer.push_excerpts(
            buffer_1.clone(),
            [
                ExcerptRange {
                    context: Point::new(0, 0)..Point::new(3, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(5, 0)..Point::new(7, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(9, 0)..Point::new(10, 4),
                    primary: None,
                },
            ],
            cx,
        );
        multibuffer.push_excerpts(
            buffer_2.clone(),
            [
                ExcerptRange {
                    context: Point::new(0, 0)..Point::new(3, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(5, 0)..Point::new(7, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(9, 0)..Point::new(10, 4),
                    primary: None,
                },
            ],
            cx,
        );
        multibuffer.push_excerpts(
            buffer_3.clone(),
            [
                ExcerptRange {
                    context: Point::new(0, 0)..Point::new(3, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(5, 0)..Point::new(7, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(9, 0)..Point::new(10, 4),
                    primary: None,
                },
            ],
            cx,
        );
        multibuffer
    });

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(
        "/a",
        json!({
            "main.rs": sample_text_1,
            "other.rs": sample_text_2,
            "lib.rs": sample_text_3,
        }),
    )
    .await;
    let project = Project::test(fs, ["/a".as_ref()], cx).await;
    let workspace = cx.add_window(|cx| Workspace::test_new(project.clone(), cx));
    let cx = &mut VisualTestContext::from_window(*workspace.deref(), cx);
    let multi_buffer_editor = cx.new_view(|cx| {
        Editor::new(
            EditorMode::Full,
            multi_buffer,
            Some(project.clone()),
            true,
            cx,
        )
    });
    let multibuffer_item_id = workspace
        .update(cx, |workspace, cx| {
            assert!(
                workspace.active_item(cx).is_none(),
                "active item should be None before the first item is added"
            );
            workspace.add_item_to_active_pane(
                Box::new(multi_buffer_editor.clone()),
                None,
                true,
                cx,
            );
            let active_item = workspace
                .active_item(cx)
                .expect("should have an active item after adding the multi buffer");
            assert!(
                !active_item.is_singleton(cx),
                "A multi buffer was expected to active after adding"
            );
            active_item.item_id()
        })
        .unwrap();
    cx.executor().run_until_parked();

    multi_buffer_editor.update(cx, |editor, cx| {
        editor.change_selections(Some(Autoscroll::Next), cx, |s| s.select_ranges(Some(1..2)));
        editor.open_excerpts(&OpenExcerpts, cx);
    });
    cx.executor().run_until_parked();
    let first_item_id = workspace
        .update(cx, |workspace, cx| {
            let active_item = workspace
                .active_item(cx)
                .expect("should have an active item after navigating into the 1st buffer");
            let first_item_id = active_item.item_id();
            assert_ne!(
                first_item_id, multibuffer_item_id,
                "Should navigate into the 1st buffer and activate it"
            );
            assert!(
                active_item.is_singleton(cx),
                "New active item should be a singleton buffer"
            );
            assert_eq!(
                active_item
                    .act_as::<Editor>(cx)
                    .expect("should have navigated into an editor for the 1st buffer")
                    .read(cx)
                    .text(cx),
                sample_text_1
            );

            workspace
                .go_back(workspace.active_pane().downgrade(), cx)
                .detach_and_log_err(cx);

            first_item_id
        })
        .unwrap();
    cx.executor().run_until_parked();
    workspace
        .update(cx, |workspace, cx| {
            let active_item = workspace
                .active_item(cx)
                .expect("should have an active item after navigating back");
            assert_eq!(
                active_item.item_id(),
                multibuffer_item_id,
                "Should navigate back to the multi buffer"
            );
            assert!(!active_item.is_singleton(cx));
        })
        .unwrap();

    multi_buffer_editor.update(cx, |editor, cx| {
        editor.change_selections(Some(Autoscroll::Next), cx, |s| {
            s.select_ranges(Some(39..40))
        });
        editor.open_excerpts(&OpenExcerpts, cx);
    });
    cx.executor().run_until_parked();
    let second_item_id = workspace
        .update(cx, |workspace, cx| {
            let active_item = workspace
                .active_item(cx)
                .expect("should have an active item after navigating into the 2nd buffer");
            let second_item_id = active_item.item_id();
            assert_ne!(
                second_item_id, multibuffer_item_id,
                "Should navigate away from the multibuffer"
            );
            assert_ne!(
                second_item_id, first_item_id,
                "Should navigate into the 2nd buffer and activate it"
            );
            assert!(
                active_item.is_singleton(cx),
                "New active item should be a singleton buffer"
            );
            assert_eq!(
                active_item
                    .act_as::<Editor>(cx)
                    .expect("should have navigated into an editor")
                    .read(cx)
                    .text(cx),
                sample_text_2
            );

            workspace
                .go_back(workspace.active_pane().downgrade(), cx)
                .detach_and_log_err(cx);

            second_item_id
        })
        .unwrap();
    cx.executor().run_until_parked();
    workspace
        .update(cx, |workspace, cx| {
            let active_item = workspace
                .active_item(cx)
                .expect("should have an active item after navigating back from the 2nd buffer");
            assert_eq!(
                active_item.item_id(),
                multibuffer_item_id,
                "Should navigate back from the 2nd buffer to the multi buffer"
            );
            assert!(!active_item.is_singleton(cx));
        })
        .unwrap();

    multi_buffer_editor.update(cx, |editor, cx| {
        editor.change_selections(Some(Autoscroll::Next), cx, |s| {
            s.select_ranges(Some(70..70))
        });
        editor.open_excerpts(&OpenExcerpts, cx);
    });
    cx.executor().run_until_parked();
    workspace
        .update(cx, |workspace, cx| {
            let active_item = workspace
                .active_item(cx)
                .expect("should have an active item after navigating into the 3rd buffer");
            let third_item_id = active_item.item_id();
            assert_ne!(
                third_item_id, multibuffer_item_id,
                "Should navigate into the 3rd buffer and activate it"
            );
            assert_ne!(third_item_id, first_item_id);
            assert_ne!(third_item_id, second_item_id);
            assert!(
                active_item.is_singleton(cx),
                "New active item should be a singleton buffer"
            );
            assert_eq!(
                active_item
                    .act_as::<Editor>(cx)
                    .expect("should have navigated into an editor")
                    .read(cx)
                    .text(cx),
                sample_text_3
            );

            workspace
                .go_back(workspace.active_pane().downgrade(), cx)
                .detach_and_log_err(cx);
        })
        .unwrap();
    cx.executor().run_until_parked();
    workspace
        .update(cx, |workspace, cx| {
            let active_item = workspace
                .active_item(cx)
                .expect("should have an active item after navigating back from the 3rd buffer");
            assert_eq!(
                active_item.item_id(),
                multibuffer_item_id,
                "Should navigate back from the 3rd buffer to the multi buffer"
            );
            assert!(!active_item.is_singleton(cx));
        })
        .unwrap();
}

#[gpui::test]
async fn test_toggle_hunk_diff(executor: BackgroundExecutor, cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;

    let diff_base = r#"
        use some::mod;

        const A: u32 = 42;

        fn main() {
            println!("hello");

            println!("world");
        }
        "#
    .unindent();

    cx.set_state(
        &r#"
        use some::modified;

        ˇ
        fn main() {
            println!("hello there");

            println!("around the");
            println!("world");
        }
        "#
        .unindent(),
    );

    cx.set_diff_base(&diff_base);
    executor.run_until_parked();

    cx.update_editor(|editor, cx| {
        editor.go_to_next_hunk(&GoToHunk, cx);
        editor.toggle_hunk_diff(&ToggleHunkDiff, cx);
    });
    executor.run_until_parked();
    cx.assert_state_with_diff(
        r#"
          use some::modified;


          fn main() {
        -     println!("hello");
        + ˇ    println!("hello there");

              println!("around the");
              println!("world");
          }
        "#
        .unindent(),
    );

    cx.update_editor(|editor, cx| {
        for _ in 0..3 {
            editor.go_to_next_hunk(&GoToHunk, cx);
            editor.toggle_hunk_diff(&ToggleHunkDiff, cx);
        }
    });
    executor.run_until_parked();
    cx.assert_state_with_diff(
        r#"
        - use some::mod;
        + use some::modified;

        - const A: u32 = 42;
          ˇ
          fn main() {
        -     println!("hello");
        +     println!("hello there");

        +     println!("around the");
              println!("world");
          }
        "#
        .unindent(),
    );

    cx.update_editor(|editor, cx| {
        editor.cancel(&Cancel, cx);
    });

    cx.assert_state_with_diff(
        r#"
          use some::modified;

          ˇ
          fn main() {
              println!("hello there");

              println!("around the");
              println!("world");
          }
        "#
        .unindent(),
    );
}

#[gpui::test]
async fn test_diff_base_change_with_expanded_diff_hunks(
    executor: BackgroundExecutor,
    cx: &mut gpui::TestAppContext,
) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;

    let diff_base = r#"
        use some::mod1;
        use some::mod2;

        const A: u32 = 42;
        const B: u32 = 42;
        const C: u32 = 42;

        fn main() {
            println!("hello");

            println!("world");
        }
        "#
    .unindent();

    cx.set_state(
        &r#"
        use some::mod2;

        const A: u32 = 42;
        const C: u32 = 42;

        fn main(ˇ) {
            //println!("hello");

            println!("world");
            //
            //
        }
        "#
        .unindent(),
    );

    cx.set_diff_base(&diff_base);
    executor.run_until_parked();

    cx.update_editor(|editor, cx| {
        editor.expand_all_hunk_diffs(&ExpandAllHunkDiffs, cx);
    });
    executor.run_until_parked();
    cx.assert_state_with_diff(
        r#"
        - use some::mod1;
          use some::mod2;

          const A: u32 = 42;
        - const B: u32 = 42;
          const C: u32 = 42;

          fn main(ˇ) {
        -     println!("hello");
        +     //println!("hello");

              println!("world");
        +     //
        +     //
          }
        "#
        .unindent(),
    );

    cx.set_diff_base("new diff base!");
    executor.run_until_parked();
    cx.assert_state_with_diff(
        r#"
          use some::mod2;

          const A: u32 = 42;
          const C: u32 = 42;

          fn main(ˇ) {
              //println!("hello");

              println!("world");
              //
              //
          }
        "#
        .unindent(),
    );

    cx.update_editor(|editor, cx| {
        editor.expand_all_hunk_diffs(&ExpandAllHunkDiffs, cx);
    });
    executor.run_until_parked();
    cx.assert_state_with_diff(
        r#"
        - new diff base!
        + use some::mod2;
        +
        + const A: u32 = 42;
        + const C: u32 = 42;
        +
        + fn main(ˇ) {
        +     //println!("hello");
        +
        +     println!("world");
        +     //
        +     //
        + }
        "#
        .unindent(),
    );
}

#[gpui::test]
async fn test_fold_unfold_diff_hunk(executor: BackgroundExecutor, cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;

    let diff_base = r#"
        use some::mod1;
        use some::mod2;

        const A: u32 = 42;
        const B: u32 = 42;
        const C: u32 = 42;

        fn main() {
            println!("hello");

            println!("world");
        }

        fn another() {
            println!("another");
        }

        fn another2() {
            println!("another2");
        }
        "#
    .unindent();

    cx.set_state(
        &r#"
        «use some::mod2;

        const A: u32 = 42;
        const C: u32 = 42;

        fn main() {
            //println!("hello");

            println!("world");
            //
            //ˇ»
        }

        fn another() {
            println!("another");
            println!("another");
        }

            println!("another2");
        }
        "#
        .unindent(),
    );

    cx.set_diff_base(&diff_base);
    executor.run_until_parked();

    cx.update_editor(|editor, cx| {
        editor.expand_all_hunk_diffs(&ExpandAllHunkDiffs, cx);
    });
    executor.run_until_parked();

    cx.assert_state_with_diff(
        r#"
        - use some::mod1;
          «use some::mod2;

          const A: u32 = 42;
        - const B: u32 = 42;
          const C: u32 = 42;

          fn main() {
        -     println!("hello");
        +     //println!("hello");

              println!("world");
        +     //
        +     //ˇ»
          }

          fn another() {
              println!("another");
        +     println!("another");
          }

        - fn another2() {
              println!("another2");
          }
        "#
        .unindent(),
    );

    // Fold across some of the diff hunks. They should no longer appear expanded.
    cx.update_editor(|editor, cx| editor.fold_selected_ranges(&FoldSelectedRanges, cx));
    cx.executor().run_until_parked();

    // Hunks are not shown if their position is within a fold
    cx.assert_state_with_diff(
        r#"
          «use some::mod2;

          const A: u32 = 42;
          const C: u32 = 42;

          fn main() {
              //println!("hello");

              println!("world");
              //
              //ˇ»
          }

          fn another() {
              println!("another");
        +     println!("another");
          }

        - fn another2() {
              println!("another2");
          }
        "#
        .unindent(),
    );

    cx.update_editor(|editor, cx| {
        editor.select_all(&SelectAll, cx);
        editor.unfold_lines(&UnfoldLines, cx);
    });
    cx.executor().run_until_parked();

    // The deletions reappear when unfolding.
    cx.assert_state_with_diff(
        r#"
        - use some::mod1;
          «use some::mod2;

          const A: u32 = 42;
        - const B: u32 = 42;
          const C: u32 = 42;

          fn main() {
        -     println!("hello");
        +     //println!("hello");

              println!("world");
        +     //
        +     //
          }

          fn another() {
              println!("another");
        +     println!("another");
          }

        - fn another2() {
              println!("another2");
          }
          ˇ»"#
        .unindent(),
    );
}

#[gpui::test]
async fn test_toggle_diff_expand_in_multi_buffer(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let file_1_old = "aaa\nbbb\nccc\nddd\neee\nfff\nggg\nhhh\niii\njjj";
    let file_1_new = "aaa\nccc\nddd\neee\nfff\nggg\nhhh\niii\njjj";
    let file_2_old = "lll\nmmm\nnnn\nooo\nppp\nqqq\nrrr\nsss\nttt\nuuu";
    let file_2_new = "lll\nmmm\nNNN\nooo\nppp\nqqq\nrrr\nsss\nttt\nuuu";
    let file_3_old = "111\n222\n333\n444\n555\n777\n888\n999\n000\n!!!";
    let file_3_new = "111\n222\n333\n444\n555\n666\n777\n888\n999\n000\n!!!";

    let buffer_1 = cx.new_model(|cx| Buffer::local(file_1_new.to_string(), cx));
    let buffer_2 = cx.new_model(|cx| Buffer::local(file_2_new.to_string(), cx));
    let buffer_3 = cx.new_model(|cx| Buffer::local(file_3_new.to_string(), cx));

    let multi_buffer = cx.new_model(|cx| {
        let mut multibuffer = MultiBuffer::new(ReadWrite);
        multibuffer.push_excerpts(
            buffer_1.clone(),
            [
                ExcerptRange {
                    context: Point::new(0, 0)..Point::new(3, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(5, 0)..Point::new(7, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(9, 0)..Point::new(10, 3),
                    primary: None,
                },
            ],
            cx,
        );
        multibuffer.push_excerpts(
            buffer_2.clone(),
            [
                ExcerptRange {
                    context: Point::new(0, 0)..Point::new(3, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(5, 0)..Point::new(7, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(9, 0)..Point::new(10, 3),
                    primary: None,
                },
            ],
            cx,
        );
        multibuffer.push_excerpts(
            buffer_3.clone(),
            [
                ExcerptRange {
                    context: Point::new(0, 0)..Point::new(3, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(5, 0)..Point::new(7, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(9, 0)..Point::new(10, 3),
                    primary: None,
                },
            ],
            cx,
        );
        multibuffer
    });

    let editor = cx.add_window(|cx| Editor::new(EditorMode::Full, multi_buffer, None, true, cx));
    editor
        .update(cx, |editor, cx| {
            for (buffer, diff_base) in [
                (buffer_1.clone(), file_1_old),
                (buffer_2.clone(), file_2_old),
                (buffer_3.clone(), file_3_old),
            ] {
                let change_set = cx.new_model(|cx| {
                    BufferChangeSet::new_with_base_text(
                        diff_base.to_string(),
                        buffer.read(cx).text_snapshot(),
                        cx,
                    )
                });
                editor.diff_map.add_change_set(change_set, cx)
            }
        })
        .unwrap();

    let mut cx = EditorTestContext::for_editor(editor, cx).await;
    cx.run_until_parked();

    cx.assert_editor_state(
        &"
            ˇaaa
            ccc
            ddd

            ggg
            hhh


            lll
            mmm
            NNN

            qqq
            rrr

            uuu
            111
            222
            333

            666
            777

            000
            !!!"
        .unindent(),
    );

    cx.update_editor(|editor, cx| {
        editor.select_all(&SelectAll, cx);
        editor.toggle_hunk_diff(&ToggleHunkDiff, cx);
    });
    cx.executor().run_until_parked();

    cx.assert_state_with_diff(
        "
            «aaa
          - bbb
            ccc
            ddd

            ggg
            hhh


            lll
            mmm
          - nnn
          + NNN

            qqq
            rrr

            uuu
            111
            222
            333

          + 666
            777

            000
            !!!ˇ»"
            .unindent(),
    );
}

#[gpui::test]
async fn test_expand_diff_hunk_at_excerpt_boundary(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let base = "aaa\nbbb\nccc\nddd\neee\nfff\nggg\n";
    let text = "aaa\nBBB\nBB2\nccc\nDDD\nEEE\nfff\nggg\n";

    let buffer = cx.new_model(|cx| Buffer::local(text.to_string(), cx));
    let multi_buffer = cx.new_model(|cx| {
        let mut multibuffer = MultiBuffer::new(ReadWrite);
        multibuffer.push_excerpts(
            buffer.clone(),
            [
                ExcerptRange {
                    context: Point::new(0, 0)..Point::new(2, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(5, 0)..Point::new(7, 0),
                    primary: None,
                },
            ],
            cx,
        );
        multibuffer
    });

    let editor = cx.add_window(|cx| Editor::new(EditorMode::Full, multi_buffer, None, true, cx));
    editor
        .update(cx, |editor, cx| {
            let buffer = buffer.read(cx).text_snapshot();
            let change_set = cx
                .new_model(|cx| BufferChangeSet::new_with_base_text(base.to_string(), buffer, cx));
            editor.diff_map.add_change_set(change_set, cx)
        })
        .unwrap();

    let mut cx = EditorTestContext::for_editor(editor, cx).await;
    cx.run_until_parked();

    cx.update_editor(|editor, cx| editor.expand_all_hunk_diffs(&Default::default(), cx));
    cx.executor().run_until_parked();

    cx.assert_state_with_diff(
        "
            ˇaaa
          - bbb
          + BBB

          - ddd
          - eee
          + EEE
            fff
        "
        .unindent(),
    );
}

#[gpui::test]
async fn test_edits_around_expanded_insertion_hunks(
    executor: BackgroundExecutor,
    cx: &mut gpui::TestAppContext,
) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;

    let diff_base = r#"
        use some::mod1;
        use some::mod2;

        const A: u32 = 42;

        fn main() {
            println!("hello");

            println!("world");
        }
        "#
    .unindent();
    executor.run_until_parked();
    cx.set_state(
        &r#"
        use some::mod1;
        use some::mod2;

        const A: u32 = 42;
        const B: u32 = 42;
        const C: u32 = 42;
        ˇ

        fn main() {
            println!("hello");

            println!("world");
        }
        "#
        .unindent(),
    );

    cx.set_diff_base(&diff_base);
    executor.run_until_parked();

    cx.update_editor(|editor, cx| {
        editor.expand_all_hunk_diffs(&ExpandAllHunkDiffs, cx);
    });
    executor.run_until_parked();

    cx.assert_state_with_diff(
        r#"
        use some::mod1;
        use some::mod2;

        const A: u32 = 42;
      + const B: u32 = 42;
      + const C: u32 = 42;
      + ˇ

        fn main() {
            println!("hello");

            println!("world");
        }
        "#
        .unindent(),
    );

    cx.update_editor(|editor, cx| editor.handle_input("const D: u32 = 42;\n", cx));
    executor.run_until_parked();

    cx.assert_state_with_diff(
        r#"
        use some::mod1;
        use some::mod2;

        const A: u32 = 42;
      + const B: u32 = 42;
      + const C: u32 = 42;
      + const D: u32 = 42;
      + ˇ

        fn main() {
            println!("hello");

            println!("world");
        }
        "#
        .unindent(),
    );

    cx.update_editor(|editor, cx| editor.handle_input("const E: u32 = 42;\n", cx));
    executor.run_until_parked();

    cx.assert_state_with_diff(
        r#"
        use some::mod1;
        use some::mod2;

        const A: u32 = 42;
      + const B: u32 = 42;
      + const C: u32 = 42;
      + const D: u32 = 42;
      + const E: u32 = 42;
      + ˇ

        fn main() {
            println!("hello");

            println!("world");
        }
        "#
        .unindent(),
    );

    cx.update_editor(|editor, cx| {
        editor.delete_line(&DeleteLine, cx);
    });
    executor.run_until_parked();

    cx.assert_state_with_diff(
        r#"
        use some::mod1;
        use some::mod2;

        const A: u32 = 42;
      + const B: u32 = 42;
      + const C: u32 = 42;
      + const D: u32 = 42;
      + const E: u32 = 42;
        ˇ
        fn main() {
            println!("hello");

            println!("world");
        }
        "#
        .unindent(),
    );

    cx.update_editor(|editor, cx| {
        editor.move_up(&MoveUp, cx);
        editor.delete_line(&DeleteLine, cx);
        editor.move_up(&MoveUp, cx);
        editor.delete_line(&DeleteLine, cx);
        editor.move_up(&MoveUp, cx);
        editor.delete_line(&DeleteLine, cx);
    });
    executor.run_until_parked();
    cx.assert_state_with_diff(
        r#"
        use some::mod1;
        use some::mod2;

        const A: u32 = 42;
      + const B: u32 = 42;
        ˇ
        fn main() {
            println!("hello");

            println!("world");
        }
        "#
        .unindent(),
    );

    cx.update_editor(|editor, cx| {
        editor.select_up_by_lines(&SelectUpByLines { lines: 5 }, cx);
        editor.delete_line(&DeleteLine, cx);
    });
    executor.run_until_parked();
    cx.assert_state_with_diff(
        r#"
        use some::mod1;
      - use some::mod2;
      -
      - const A: u32 = 42;
        ˇ
        fn main() {
            println!("hello");

            println!("world");
        }
        "#
        .unindent(),
    );
}

#[gpui::test]
async fn test_edits_around_expanded_deletion_hunks(
    executor: BackgroundExecutor,
    cx: &mut gpui::TestAppContext,
) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;

    let diff_base = r#"
        use some::mod1;
        use some::mod2;

        const A: u32 = 42;
        const B: u32 = 42;
        const C: u32 = 42;


        fn main() {
            println!("hello");

            println!("world");
        }
    "#
    .unindent();
    executor.run_until_parked();
    cx.set_state(
        &r#"
        use some::mod1;
        use some::mod2;

        ˇconst B: u32 = 42;
        const C: u32 = 42;


        fn main() {
            println!("hello");

            println!("world");
        }
        "#
        .unindent(),
    );

    cx.set_diff_base(&diff_base);
    executor.run_until_parked();

    cx.update_editor(|editor, cx| {
        editor.expand_all_hunk_diffs(&ExpandAllHunkDiffs, cx);
    });
    executor.run_until_parked();

    cx.assert_state_with_diff(
        r#"
        use some::mod1;
        use some::mod2;

      - const A: u32 = 42;
        ˇconst B: u32 = 42;
        const C: u32 = 42;


        fn main() {
            println!("hello");

            println!("world");
        }
        "#
        .unindent(),
    );

    cx.update_editor(|editor, cx| {
        editor.delete_line(&DeleteLine, cx);
    });
    executor.run_until_parked();
    cx.assert_state_with_diff(
        r#"
        use some::mod1;
        use some::mod2;

      - const A: u32 = 42;
      - const B: u32 = 42;
        ˇconst C: u32 = 42;


        fn main() {
            println!("hello");

            println!("world");
        }
        "#
        .unindent(),
    );

    cx.update_editor(|editor, cx| {
        editor.delete_line(&DeleteLine, cx);
    });
    executor.run_until_parked();
    cx.assert_state_with_diff(
        r#"
        use some::mod1;
        use some::mod2;

      - const A: u32 = 42;
      - const B: u32 = 42;
      - const C: u32 = 42;
        ˇ

        fn main() {
            println!("hello");

            println!("world");
        }
        "#
        .unindent(),
    );

    cx.update_editor(|editor, cx| {
        editor.handle_input("replacement", cx);
    });
    executor.run_until_parked();
    cx.assert_state_with_diff(
        r#"
        use some::mod1;
        use some::mod2;

      - const A: u32 = 42;
      - const B: u32 = 42;
      - const C: u32 = 42;
      -
      + replacementˇ

        fn main() {
            println!("hello");

            println!("world");
        }
        "#
        .unindent(),
    );
}

#[gpui::test]
async fn test_edit_after_expanded_modification_hunk(
    executor: BackgroundExecutor,
    cx: &mut gpui::TestAppContext,
) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;

    let diff_base = r#"
        use some::mod1;
        use some::mod2;

        const A: u32 = 42;
        const B: u32 = 42;
        const C: u32 = 42;
        const D: u32 = 42;


        fn main() {
            println!("hello");

            println!("world");
        }"#
    .unindent();

    cx.set_state(
        &r#"
        use some::mod1;
        use some::mod2;

        const A: u32 = 42;
        const B: u32 = 42;
        const C: u32 = 43ˇ
        const D: u32 = 42;


        fn main() {
            println!("hello");

            println!("world");
        }"#
        .unindent(),
    );

    cx.set_diff_base(&diff_base);
    executor.run_until_parked();
    cx.update_editor(|editor, cx| {
        editor.expand_all_hunk_diffs(&ExpandAllHunkDiffs, cx);
    });
    executor.run_until_parked();

    cx.assert_state_with_diff(
        r#"
        use some::mod1;
        use some::mod2;

        const A: u32 = 42;
        const B: u32 = 42;
      - const C: u32 = 42;
      + const C: u32 = 43ˇ
        const D: u32 = 42;


        fn main() {
            println!("hello");

            println!("world");
        }"#
        .unindent(),
    );

    cx.update_editor(|editor, cx| {
        editor.handle_input("\nnew_line\n", cx);
    });
    executor.run_until_parked();

    cx.assert_state_with_diff(
        r#"
        use some::mod1;
        use some::mod2;

        const A: u32 = 42;
        const B: u32 = 42;
      - const C: u32 = 42;
      + const C: u32 = 43
      + new_line
      + ˇ
        const D: u32 = 42;


        fn main() {
            println!("hello");

            println!("world");
        }"#
        .unindent(),
    );
}

async fn setup_indent_guides_editor(
    text: &str,
    cx: &mut gpui::TestAppContext,
) -> (BufferId, EditorTestContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;

    let buffer_id = cx.update_editor(|editor, cx| {
        editor.set_text(text, cx);
        let buffer_ids = editor.buffer().read(cx).excerpt_buffer_ids();

        buffer_ids[0]
    });

    (buffer_id, cx)
}

fn assert_indent_guides(
    range: Range<u32>,
    expected: Vec<IndentGuide>,
    active_indices: Option<Vec<usize>>,
    cx: &mut EditorTestContext,
) {
    let indent_guides = cx.update_editor(|editor, cx| {
        let snapshot = editor.snapshot(cx).display_snapshot;
        let mut indent_guides: Vec<_> = crate::indent_guides::indent_guides_in_range(
            editor,
            MultiBufferRow(range.start)..MultiBufferRow(range.end),
            true,
            &snapshot,
            cx,
        );

        indent_guides.sort_by(|a, b| {
            a.depth.cmp(&b.depth).then(
                a.start_row
                    .cmp(&b.start_row)
                    .then(a.end_row.cmp(&b.end_row)),
            )
        });
        indent_guides
    });

    if let Some(expected) = active_indices {
        let active_indices = cx.update_editor(|editor, cx| {
            let snapshot = editor.snapshot(cx).display_snapshot;
            editor.find_active_indent_guide_indices(&indent_guides, &snapshot, cx)
        });

        assert_eq!(
            active_indices.unwrap().into_iter().collect::<Vec<_>>(),
            expected,
            "Active indent guide indices do not match"
        );
    }

    let expected: Vec<_> = expected
        .into_iter()
        .map(|guide| MultiBufferIndentGuide {
            multibuffer_row_range: MultiBufferRow(guide.start_row)..MultiBufferRow(guide.end_row),
            buffer: guide,
        })
        .collect();

    assert_eq!(indent_guides, expected, "Indent guides do not match");
}

fn indent_guide(buffer_id: BufferId, start_row: u32, end_row: u32, depth: u32) -> IndentGuide {
    IndentGuide {
        buffer_id,
        start_row,
        end_row,
        depth,
        tab_size: 4,
        settings: IndentGuideSettings {
            enabled: true,
            line_width: 1,
            active_line_width: 1,
            ..Default::default()
        },
    }
}

#[gpui::test]
async fn test_indent_guide_single_line(cx: &mut gpui::TestAppContext) {
    let (buffer_id, mut cx) = setup_indent_guides_editor(
        &"
    fn main() {
        let a = 1;
    }"
        .unindent(),
        cx,
    )
    .await;

    assert_indent_guides(0..3, vec![indent_guide(buffer_id, 1, 1, 0)], None, &mut cx);
}

#[gpui::test]
async fn test_indent_guide_simple_block(cx: &mut gpui::TestAppContext) {
    let (buffer_id, mut cx) = setup_indent_guides_editor(
        &"
    fn main() {
        let a = 1;
        let b = 2;
    }"
        .unindent(),
        cx,
    )
    .await;

    assert_indent_guides(0..4, vec![indent_guide(buffer_id, 1, 2, 0)], None, &mut cx);
}

#[gpui::test]
async fn test_indent_guide_nested(cx: &mut gpui::TestAppContext) {
    let (buffer_id, mut cx) = setup_indent_guides_editor(
        &"
    fn main() {
        let a = 1;
        if a == 3 {
            let b = 2;
        } else {
            let c = 3;
        }
    }"
        .unindent(),
        cx,
    )
    .await;

    assert_indent_guides(
        0..8,
        vec![
            indent_guide(buffer_id, 1, 6, 0),
            indent_guide(buffer_id, 3, 3, 1),
            indent_guide(buffer_id, 5, 5, 1),
        ],
        None,
        &mut cx,
    );
}

#[gpui::test]
async fn test_indent_guide_tab(cx: &mut gpui::TestAppContext) {
    let (buffer_id, mut cx) = setup_indent_guides_editor(
        &"
    fn main() {
        let a = 1;
            let b = 2;
        let c = 3;
    }"
        .unindent(),
        cx,
    )
    .await;

    assert_indent_guides(
        0..5,
        vec![
            indent_guide(buffer_id, 1, 3, 0),
            indent_guide(buffer_id, 2, 2, 1),
        ],
        None,
        &mut cx,
    );
}

#[gpui::test]
async fn test_indent_guide_continues_on_empty_line(cx: &mut gpui::TestAppContext) {
    let (buffer_id, mut cx) = setup_indent_guides_editor(
        &"
        fn main() {
            let a = 1;

            let c = 3;
        }"
        .unindent(),
        cx,
    )
    .await;

    assert_indent_guides(0..5, vec![indent_guide(buffer_id, 1, 3, 0)], None, &mut cx);
}

#[gpui::test]
async fn test_indent_guide_complex(cx: &mut gpui::TestAppContext) {
    let (buffer_id, mut cx) = setup_indent_guides_editor(
        &"
        fn main() {
            let a = 1;

            let c = 3;

            if a == 3 {
                let b = 2;
            } else {
                let c = 3;
            }
        }"
        .unindent(),
        cx,
    )
    .await;

    assert_indent_guides(
        0..11,
        vec![
            indent_guide(buffer_id, 1, 9, 0),
            indent_guide(buffer_id, 6, 6, 1),
            indent_guide(buffer_id, 8, 8, 1),
        ],
        None,
        &mut cx,
    );
}

#[gpui::test]
async fn test_indent_guide_starts_off_screen(cx: &mut gpui::TestAppContext) {
    let (buffer_id, mut cx) = setup_indent_guides_editor(
        &"
        fn main() {
            let a = 1;

            let c = 3;

            if a == 3 {
                let b = 2;
            } else {
                let c = 3;
            }
        }"
        .unindent(),
        cx,
    )
    .await;

    assert_indent_guides(
        1..11,
        vec![
            indent_guide(buffer_id, 1, 9, 0),
            indent_guide(buffer_id, 6, 6, 1),
            indent_guide(buffer_id, 8, 8, 1),
        ],
        None,
        &mut cx,
    );
}

#[gpui::test]
async fn test_indent_guide_ends_off_screen(cx: &mut gpui::TestAppContext) {
    let (buffer_id, mut cx) = setup_indent_guides_editor(
        &"
        fn main() {
            let a = 1;

            let c = 3;

            if a == 3 {
                let b = 2;
            } else {
                let c = 3;
            }
        }"
        .unindent(),
        cx,
    )
    .await;

    assert_indent_guides(
        1..10,
        vec![
            indent_guide(buffer_id, 1, 9, 0),
            indent_guide(buffer_id, 6, 6, 1),
            indent_guide(buffer_id, 8, 8, 1),
        ],
        None,
        &mut cx,
    );
}

#[gpui::test]
async fn test_indent_guide_without_brackets(cx: &mut gpui::TestAppContext) {
    let (buffer_id, mut cx) = setup_indent_guides_editor(
        &"
        block1
            block2
                block3
                    block4
            block2
        block1
        block1"
            .unindent(),
        cx,
    )
    .await;

    assert_indent_guides(
        1..10,
        vec![
            indent_guide(buffer_id, 1, 4, 0),
            indent_guide(buffer_id, 2, 3, 1),
            indent_guide(buffer_id, 3, 3, 2),
        ],
        None,
        &mut cx,
    );
}

#[gpui::test]
async fn test_indent_guide_ends_before_empty_line(cx: &mut gpui::TestAppContext) {
    let (buffer_id, mut cx) = setup_indent_guides_editor(
        &"
        block1
            block2
                block3

        block1
        block1"
            .unindent(),
        cx,
    )
    .await;

    assert_indent_guides(
        0..6,
        vec![
            indent_guide(buffer_id, 1, 2, 0),
            indent_guide(buffer_id, 2, 2, 1),
        ],
        None,
        &mut cx,
    );
}

#[gpui::test]
async fn test_indent_guide_continuing_off_screen(cx: &mut gpui::TestAppContext) {
    let (buffer_id, mut cx) = setup_indent_guides_editor(
        &"
        block1



            block2
        "
        .unindent(),
        cx,
    )
    .await;

    assert_indent_guides(0..1, vec![indent_guide(buffer_id, 1, 1, 0)], None, &mut cx);
}

#[gpui::test]
async fn test_indent_guide_tabs(cx: &mut gpui::TestAppContext) {
    let (buffer_id, mut cx) = setup_indent_guides_editor(
        &"
        def a:
        \tb = 3
        \tif True:
        \t\tc = 4
        \t\td = 5
        \tprint(b)
        "
        .unindent(),
        cx,
    )
    .await;

    assert_indent_guides(
        0..6,
        vec![
            indent_guide(buffer_id, 1, 6, 0),
            indent_guide(buffer_id, 3, 4, 1),
        ],
        None,
        &mut cx,
    );
}

#[gpui::test]
async fn test_active_indent_guide_single_line(cx: &mut gpui::TestAppContext) {
    let (buffer_id, mut cx) = setup_indent_guides_editor(
        &"
    fn main() {
        let a = 1;
    }"
        .unindent(),
        cx,
    )
    .await;

    cx.update_editor(|editor, cx| {
        editor.change_selections(None, cx, |s| {
            s.select_ranges([Point::new(1, 0)..Point::new(1, 0)])
        });
    });

    assert_indent_guides(
        0..3,
        vec![indent_guide(buffer_id, 1, 1, 0)],
        Some(vec![0]),
        &mut cx,
    );
}

#[gpui::test]
async fn test_active_indent_guide_respect_indented_range(cx: &mut gpui::TestAppContext) {
    let (buffer_id, mut cx) = setup_indent_guides_editor(
        &"
    fn main() {
        if 1 == 2 {
            let a = 1;
        }
    }"
        .unindent(),
        cx,
    )
    .await;

    cx.update_editor(|editor, cx| {
        editor.change_selections(None, cx, |s| {
            s.select_ranges([Point::new(1, 0)..Point::new(1, 0)])
        });
    });

    assert_indent_guides(
        0..4,
        vec![
            indent_guide(buffer_id, 1, 3, 0),
            indent_guide(buffer_id, 2, 2, 1),
        ],
        Some(vec![1]),
        &mut cx,
    );

    cx.update_editor(|editor, cx| {
        editor.change_selections(None, cx, |s| {
            s.select_ranges([Point::new(2, 0)..Point::new(2, 0)])
        });
    });

    assert_indent_guides(
        0..4,
        vec![
            indent_guide(buffer_id, 1, 3, 0),
            indent_guide(buffer_id, 2, 2, 1),
        ],
        Some(vec![1]),
        &mut cx,
    );

    cx.update_editor(|editor, cx| {
        editor.change_selections(None, cx, |s| {
            s.select_ranges([Point::new(3, 0)..Point::new(3, 0)])
        });
    });

    assert_indent_guides(
        0..4,
        vec![
            indent_guide(buffer_id, 1, 3, 0),
            indent_guide(buffer_id, 2, 2, 1),
        ],
        Some(vec![0]),
        &mut cx,
    );
}

#[gpui::test]
async fn test_active_indent_guide_empty_line(cx: &mut gpui::TestAppContext) {
    let (buffer_id, mut cx) = setup_indent_guides_editor(
        &"
    fn main() {
        let a = 1;

        let b = 2;
    }"
        .unindent(),
        cx,
    )
    .await;

    cx.update_editor(|editor, cx| {
        editor.change_selections(None, cx, |s| {
            s.select_ranges([Point::new(2, 0)..Point::new(2, 0)])
        });
    });

    assert_indent_guides(
        0..5,
        vec![indent_guide(buffer_id, 1, 3, 0)],
        Some(vec![0]),
        &mut cx,
    );
}

#[gpui::test]
async fn test_active_indent_guide_non_matching_indent(cx: &mut gpui::TestAppContext) {
    let (buffer_id, mut cx) = setup_indent_guides_editor(
        &"
    def m:
        a = 1
        pass"
            .unindent(),
        cx,
    )
    .await;

    cx.update_editor(|editor, cx| {
        editor.change_selections(None, cx, |s| {
            s.select_ranges([Point::new(1, 0)..Point::new(1, 0)])
        });
    });

    assert_indent_guides(
        0..3,
        vec![indent_guide(buffer_id, 1, 2, 0)],
        Some(vec![0]),
        &mut cx,
    );
}

#[gpui::test]
fn test_crease_insertion_and_rendering(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    let editor = cx.add_window(|cx| {
        let buffer = MultiBuffer::build_simple("aaaaaa\nbbbbbb\ncccccc\nddddddd\n", cx);
        build_editor(buffer, cx)
    });

    let render_args = Arc::new(Mutex::new(None));
    let snapshot = editor
        .update(cx, |editor, cx| {
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            let range =
                snapshot.anchor_before(Point::new(1, 0))..snapshot.anchor_after(Point::new(2, 6));

            struct RenderArgs {
                row: MultiBufferRow,
                folded: bool,
                callback: Arc<dyn Fn(bool, &mut WindowContext) + Send + Sync>,
            }

            let crease = Crease::inline(
                range,
                FoldPlaceholder::test(),
                {
                    let toggle_callback = render_args.clone();
                    move |row, folded, callback, _cx| {
                        *toggle_callback.lock() = Some(RenderArgs {
                            row,
                            folded,
                            callback,
                        });
                        div()
                    }
                },
                |_row, _folded, _cx| div(),
            );

            editor.insert_creases(Some(crease), cx);
            let snapshot = editor.snapshot(cx);
            let _div =
                snapshot.render_crease_toggle(MultiBufferRow(1), false, cx.view().clone(), cx);
            snapshot
        })
        .unwrap();

    let render_args = render_args.lock().take().unwrap();
    assert_eq!(render_args.row, MultiBufferRow(1));
    assert!(!render_args.folded);
    assert!(!snapshot.is_line_folded(MultiBufferRow(1)));

    cx.update_window(*editor, |_, cx| (render_args.callback)(true, cx))
        .unwrap();
    let snapshot = editor.update(cx, |editor, cx| editor.snapshot(cx)).unwrap();
    assert!(snapshot.is_line_folded(MultiBufferRow(1)));

    cx.update_window(*editor, |_, cx| (render_args.callback)(false, cx))
        .unwrap();
    let snapshot = editor.update(cx, |editor, cx| editor.snapshot(cx)).unwrap();
    assert!(!snapshot.is_line_folded(MultiBufferRow(1)));
}

#[gpui::test]
async fn test_input_text(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});
    let mut cx = EditorTestContext::new(cx).await;

    cx.set_state(
        &r#"ˇone
        two

        three
        fourˇ
        five

        siˇx"#
            .unindent(),
    );

    cx.dispatch_action(HandleInput(String::new()));
    cx.assert_editor_state(
        &r#"ˇone
        two

        three
        fourˇ
        five

        siˇx"#
            .unindent(),
    );

    cx.dispatch_action(HandleInput("AAAA".to_string()));
    cx.assert_editor_state(
        &r#"AAAAˇone
        two

        three
        fourAAAAˇ
        five

        siAAAAˇx"#
            .unindent(),
    );
}

#[gpui::test]
async fn test_scroll_cursor_center_top_bottom(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let mut cx = EditorTestContext::new(cx).await;
    cx.set_state(
        r#"let foo = 1;
let foo = 2;
let foo = 3;
let fooˇ = 4;
let foo = 5;
let foo = 6;
let foo = 7;
let foo = 8;
let foo = 9;
let foo = 10;
let foo = 11;
let foo = 12;
let foo = 13;
let foo = 14;
let foo = 15;"#,
    );

    cx.update_editor(|e, cx| {
        assert_eq!(
            e.next_scroll_position,
            NextScrollCursorCenterTopBottom::Center,
            "Default next scroll direction is center",
        );

        e.scroll_cursor_center_top_bottom(&ScrollCursorCenterTopBottom, cx);
        assert_eq!(
            e.next_scroll_position,
            NextScrollCursorCenterTopBottom::Top,
            "After center, next scroll direction should be top",
        );

        e.scroll_cursor_center_top_bottom(&ScrollCursorCenterTopBottom, cx);
        assert_eq!(
            e.next_scroll_position,
            NextScrollCursorCenterTopBottom::Bottom,
            "After top, next scroll direction should be bottom",
        );

        e.scroll_cursor_center_top_bottom(&ScrollCursorCenterTopBottom, cx);
        assert_eq!(
            e.next_scroll_position,
            NextScrollCursorCenterTopBottom::Center,
            "After bottom, scrolling should start over",
        );

        e.scroll_cursor_center_top_bottom(&ScrollCursorCenterTopBottom, cx);
        assert_eq!(
            e.next_scroll_position,
            NextScrollCursorCenterTopBottom::Top,
            "Scrolling continues if retriggered fast enough"
        );
    });

    cx.executor()
        .advance_clock(SCROLL_CENTER_TOP_BOTTOM_DEBOUNCE_TIMEOUT + Duration::from_millis(200));
    cx.executor().run_until_parked();
    cx.update_editor(|e, _| {
        assert_eq!(
            e.next_scroll_position,
            NextScrollCursorCenterTopBottom::Center,
            "If scrolling is not triggered fast enough, it should reset"
        );
    });
}

#[gpui::test]
async fn test_goto_definition_with_find_all_references_fallback(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});
    let mut cx = EditorLspTestContext::new_rust(
        lsp::ServerCapabilities {
            definition_provider: Some(lsp::OneOf::Left(true)),
            references_provider: Some(lsp::OneOf::Left(true)),
            ..lsp::ServerCapabilities::default()
        },
        cx,
    )
    .await;

    let set_up_lsp_handlers = |empty_go_to_definition: bool, cx: &mut EditorLspTestContext| {
        let go_to_definition = cx.lsp.handle_request::<lsp::request::GotoDefinition, _, _>(
            move |params, _| async move {
                if empty_go_to_definition {
                    Ok(None)
                } else {
                    Ok(Some(lsp::GotoDefinitionResponse::Scalar(lsp::Location {
                        uri: params.text_document_position_params.text_document.uri,
                        range: lsp::Range::new(lsp::Position::new(4, 3), lsp::Position::new(4, 6)),
                    })))
                }
            },
        );
        let references =
            cx.lsp
                .handle_request::<lsp::request::References, _, _>(move |params, _| async move {
                    Ok(Some(vec![lsp::Location {
                        uri: params.text_document_position.text_document.uri,
                        range: lsp::Range::new(lsp::Position::new(0, 8), lsp::Position::new(0, 11)),
                    }]))
                });
        (go_to_definition, references)
    };

    cx.set_state(
        &r#"fn one() {
            let mut a = ˇtwo();
        }

        fn two() {}"#
            .unindent(),
    );
    set_up_lsp_handlers(false, &mut cx);
    let navigated = cx
        .update_editor(|editor, cx| editor.go_to_definition(&GoToDefinition, cx))
        .await
        .expect("Failed to navigate to definition");
    assert_eq!(
        navigated,
        Navigated::Yes,
        "Should have navigated to definition from the GetDefinition response"
    );
    cx.assert_editor_state(
        &r#"fn one() {
            let mut a = two();
        }

        fn «twoˇ»() {}"#
            .unindent(),
    );

    let editors = cx.update_workspace(|workspace, cx| {
        workspace.items_of_type::<Editor>(cx).collect::<Vec<_>>()
    });
    cx.update_editor(|_, test_editor_cx| {
        assert_eq!(
            editors.len(),
            1,
            "Initially, only one, test, editor should be open in the workspace"
        );
        assert_eq!(
            test_editor_cx.view(),
            editors.last().expect("Asserted len is 1")
        );
    });

    set_up_lsp_handlers(true, &mut cx);
    let navigated = cx
        .update_editor(|editor, cx| editor.go_to_definition(&GoToDefinition, cx))
        .await
        .expect("Failed to navigate to lookup references");
    assert_eq!(
        navigated,
        Navigated::Yes,
        "Should have navigated to references as a fallback after empty GoToDefinition response"
    );
    // We should not change the selections in the existing file,
    // if opening another milti buffer with the references
    cx.assert_editor_state(
        &r#"fn one() {
            let mut a = two();
        }

        fn «twoˇ»() {}"#
            .unindent(),
    );
    let editors = cx.update_workspace(|workspace, cx| {
        workspace.items_of_type::<Editor>(cx).collect::<Vec<_>>()
    });
    cx.update_editor(|_, test_editor_cx| {
        assert_eq!(
            editors.len(),
            2,
            "After falling back to references search, we open a new editor with the results"
        );
        let references_fallback_text = editors
            .into_iter()
            .find(|new_editor| new_editor != test_editor_cx.view())
            .expect("Should have one non-test editor now")
            .read(test_editor_cx)
            .text(test_editor_cx);
        assert_eq!(
            references_fallback_text, "fn one() {\n    let mut a = two();\n}",
            "Should use the range from the references response and not the GoToDefinition one"
        );
    });
}

#[gpui::test]
async fn test_find_enclosing_node_with_task(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let language = Arc::new(Language::new(
        LanguageConfig::default(),
        Some(tree_sitter_rust::LANGUAGE.into()),
    ));

    let text = r#"
        #[cfg(test)]
        mod tests() {
            #[test]
            fn runnable_1() {
                let a = 1;
            }

            #[test]
            fn runnable_2() {
                let a = 1;
                let b = 2;
            }
        }
    "#
    .unindent();

    let fs = FakeFs::new(cx.executor());
    fs.insert_file("/file.rs", Default::default()).await;

    let project = Project::test(fs, ["/a".as_ref()], cx).await;
    let workspace = cx.add_window(|cx| Workspace::test_new(project.clone(), cx));
    let cx = &mut VisualTestContext::from_window(*workspace.deref(), cx);
    let buffer = cx.new_model(|cx| Buffer::local(text, cx).with_language(language, cx));
    let multi_buffer = cx.new_model(|cx| MultiBuffer::singleton(buffer.clone(), cx));

    let editor = cx.new_view(|cx| {
        Editor::new(
            EditorMode::Full,
            multi_buffer,
            Some(project.clone()),
            true,
            cx,
        )
    });

    editor.update(cx, |editor, cx| {
        editor.tasks.insert(
            (buffer.read(cx).remote_id(), 3),
            RunnableTasks {
                templates: vec![],
                offset: MultiBufferOffset(43),
                column: 0,
                extra_variables: HashMap::default(),
                context_range: BufferOffset(43)..BufferOffset(85),
            },
        );
        editor.tasks.insert(
            (buffer.read(cx).remote_id(), 8),
            RunnableTasks {
                templates: vec![],
                offset: MultiBufferOffset(86),
                column: 0,
                extra_variables: HashMap::default(),
                context_range: BufferOffset(86)..BufferOffset(191),
            },
        );

        // Test finding task when cursor is inside function body
        editor.change_selections(None, cx, |s| {
            s.select_ranges([Point::new(4, 5)..Point::new(4, 5)])
        });
        let (_, row, _) = editor.find_enclosing_node_task(cx).unwrap();
        assert_eq!(row, 3, "Should find task for cursor inside runnable_1");

        // Test finding task when cursor is on function name
        editor.change_selections(None, cx, |s| {
            s.select_ranges([Point::new(8, 4)..Point::new(8, 4)])
        });
        let (_, row, _) = editor.find_enclosing_node_task(cx).unwrap();
        assert_eq!(row, 8, "Should find task when cursor is on function name");
    });
}

#[gpui::test]
async fn test_multi_buffer_folding(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let sample_text_1 = "aaaa\nbbbb\ncccc\ndddd\neeee\nffff\ngggg\nhhhh\niiii\njjjj".to_string();
    let sample_text_2 = "llll\nmmmm\nnnnn\noooo\npppp\nqqqq\nrrrr\nssss\ntttt\nuuuu".to_string();
    let sample_text_3 = "vvvv\nwwww\nxxxx\nyyyy\nzzzz\n1111\n2222\n3333\n4444\n5555".to_string();

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(
        "/a",
        json!({
            "first.rs": sample_text_1,
            "second.rs": sample_text_2,
            "third.rs": sample_text_3,
        }),
    )
    .await;
    let project = Project::test(fs, ["/a".as_ref()], cx).await;
    let workspace = cx.add_window(|cx| Workspace::test_new(project.clone(), cx));
    let cx = &mut VisualTestContext::from_window(*workspace.deref(), cx);
    let worktree = project.update(cx, |project, cx| {
        let mut worktrees = project.worktrees(cx).collect::<Vec<_>>();
        assert_eq!(worktrees.len(), 1);
        worktrees.pop().unwrap()
    });
    let worktree_id = worktree.update(cx, |worktree, _| worktree.id());

    let buffer_1 = project
        .update(cx, |project, cx| {
            project.open_buffer((worktree_id, "first.rs"), cx)
        })
        .await
        .unwrap();
    let buffer_2 = project
        .update(cx, |project, cx| {
            project.open_buffer((worktree_id, "second.rs"), cx)
        })
        .await
        .unwrap();
    let buffer_3 = project
        .update(cx, |project, cx| {
            project.open_buffer((worktree_id, "third.rs"), cx)
        })
        .await
        .unwrap();

    let multi_buffer = cx.new_model(|cx| {
        let mut multi_buffer = MultiBuffer::new(ReadWrite);
        multi_buffer.push_excerpts(
            buffer_1.clone(),
            [
                ExcerptRange {
                    context: Point::new(0, 0)..Point::new(3, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(5, 0)..Point::new(7, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(9, 0)..Point::new(10, 4),
                    primary: None,
                },
            ],
            cx,
        );
        multi_buffer.push_excerpts(
            buffer_2.clone(),
            [
                ExcerptRange {
                    context: Point::new(0, 0)..Point::new(3, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(5, 0)..Point::new(7, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(9, 0)..Point::new(10, 4),
                    primary: None,
                },
            ],
            cx,
        );
        multi_buffer.push_excerpts(
            buffer_3.clone(),
            [
                ExcerptRange {
                    context: Point::new(0, 0)..Point::new(3, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(5, 0)..Point::new(7, 0),
                    primary: None,
                },
                ExcerptRange {
                    context: Point::new(9, 0)..Point::new(10, 4),
                    primary: None,
                },
            ],
            cx,
        );
        multi_buffer
    });
    let multi_buffer_editor = cx.new_view(|cx| {
        Editor::new(
            EditorMode::Full,
            multi_buffer,
            Some(project.clone()),
            true,
            cx,
        )
    });

    let full_text = "\n\n\naaaa\nbbbb\ncccc\n\n\n\nffff\ngggg\n\n\n\njjjj\n\n\n\n\nllll\nmmmm\nnnnn\n\n\n\nqqqq\nrrrr\n\n\n\nuuuu\n\n\n\n\nvvvv\nwwww\nxxxx\n\n\n\n1111\n2222\n\n\n\n5555\n";
    assert_eq!(
        multi_buffer_editor.update(cx, |editor, cx| editor.display_text(cx)),
        full_text,
    );

    multi_buffer_editor.update(cx, |editor, cx| {
        editor.fold_buffer(buffer_1.read(cx).remote_id(), cx)
    });
    assert_eq!(
        multi_buffer_editor.update(cx, |editor, cx| editor.display_text(cx)),
        "\n\n\n\n\nllll\nmmmm\nnnnn\n\n\n\nqqqq\nrrrr\n\n\n\nuuuu\n\n\n\n\nvvvv\nwwww\nxxxx\n\n\n\n1111\n2222\n\n\n\n5555\n",
        "After folding the first buffer, its text should not be displayed"
    );

    multi_buffer_editor.update(cx, |editor, cx| {
        editor.fold_buffer(buffer_2.read(cx).remote_id(), cx)
    });
    assert_eq!(
        multi_buffer_editor.update(cx, |editor, cx| editor.display_text(cx)),
        "\n\n\n\n\n\n\nvvvv\nwwww\nxxxx\n\n\n\n1111\n2222\n\n\n\n5555\n",
        "After folding the second buffer, its text should not be displayed"
    );

    multi_buffer_editor.update(cx, |editor, cx| {
        editor.fold_buffer(buffer_3.read(cx).remote_id(), cx)
    });
    assert_eq!(
        multi_buffer_editor.update(cx, |editor, cx| editor.display_text(cx)),
        "\n\n\n\n\n",
        "After folding the third buffer, its text should not be displayed"
    );

    // Emulate selection inside the fold logic, that should work
    multi_buffer_editor.update(cx, |editor, cx| {
        editor.snapshot(cx).next_line_boundary(Point::new(0, 4));
    });

    multi_buffer_editor.update(cx, |editor, cx| {
        editor.unfold_buffer(buffer_2.read(cx).remote_id(), cx)
    });
    assert_eq!(
        multi_buffer_editor.update(cx, |editor, cx| editor.display_text(cx)),
        "\n\n\n\n\nllll\nmmmm\nnnnn\n\n\n\nqqqq\nrrrr\n\n\n\nuuuu\n\n\n",
        "After unfolding the second buffer, its text should be displayed"
    );

    multi_buffer_editor.update(cx, |editor, cx| {
        editor.unfold_buffer(buffer_1.read(cx).remote_id(), cx)
    });
    assert_eq!(
        multi_buffer_editor.update(cx, |editor, cx| editor.display_text(cx)),
        "\n\n\naaaa\nbbbb\ncccc\n\n\n\nffff\ngggg\n\n\n\njjjj\n\n\n\n\nllll\nmmmm\nnnnn\n\n\n\nqqqq\nrrrr\n\n\n\nuuuu\n\n\n",
        "After unfolding the first buffer, its and 2nd buffer's text should be displayed"
    );

    multi_buffer_editor.update(cx, |editor, cx| {
        editor.unfold_buffer(buffer_3.read(cx).remote_id(), cx)
    });
    assert_eq!(
        multi_buffer_editor.update(cx, |editor, cx| editor.display_text(cx)),
        full_text,
        "After unfolding the all buffers, all original text should be displayed"
    );
}

#[gpui::test]
async fn test_multi_buffer_single_excerpts_folding(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let sample_text_1 = "1111\n2222\n3333".to_string();
    let sample_text_2 = "4444\n5555\n6666".to_string();
    let sample_text_3 = "7777\n8888\n9999".to_string();

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(
        "/a",
        json!({
            "first.rs": sample_text_1,
            "second.rs": sample_text_2,
            "third.rs": sample_text_3,
        }),
    )
    .await;
    let project = Project::test(fs, ["/a".as_ref()], cx).await;
    let workspace = cx.add_window(|cx| Workspace::test_new(project.clone(), cx));
    let cx = &mut VisualTestContext::from_window(*workspace.deref(), cx);
    let worktree = project.update(cx, |project, cx| {
        let mut worktrees = project.worktrees(cx).collect::<Vec<_>>();
        assert_eq!(worktrees.len(), 1);
        worktrees.pop().unwrap()
    });
    let worktree_id = worktree.update(cx, |worktree, _| worktree.id());

    let buffer_1 = project
        .update(cx, |project, cx| {
            project.open_buffer((worktree_id, "first.rs"), cx)
        })
        .await
        .unwrap();
    let buffer_2 = project
        .update(cx, |project, cx| {
            project.open_buffer((worktree_id, "second.rs"), cx)
        })
        .await
        .unwrap();
    let buffer_3 = project
        .update(cx, |project, cx| {
            project.open_buffer((worktree_id, "third.rs"), cx)
        })
        .await
        .unwrap();

    let multi_buffer = cx.new_model(|cx| {
        let mut multi_buffer = MultiBuffer::new(ReadWrite);
        multi_buffer.push_excerpts(
            buffer_1.clone(),
            [ExcerptRange {
                context: Point::new(0, 0)..Point::new(3, 0),
                primary: None,
            }],
            cx,
        );
        multi_buffer.push_excerpts(
            buffer_2.clone(),
            [ExcerptRange {
                context: Point::new(0, 0)..Point::new(3, 0),
                primary: None,
            }],
            cx,
        );
        multi_buffer.push_excerpts(
            buffer_3.clone(),
            [ExcerptRange {
                context: Point::new(0, 0)..Point::new(3, 0),
                primary: None,
            }],
            cx,
        );
        multi_buffer
    });

    let multi_buffer_editor = cx.new_view(|cx| {
        Editor::new(
            EditorMode::Full,
            multi_buffer,
            Some(project.clone()),
            true,
            cx,
        )
    });

    let full_text = "\n\n\n1111\n2222\n3333\n\n\n\n\n4444\n5555\n6666\n\n\n\n\n7777\n8888\n9999\n";
    assert_eq!(
        multi_buffer_editor.update(cx, |editor, cx| editor.display_text(cx)),
        full_text,
    );

    multi_buffer_editor.update(cx, |editor, cx| {
        editor.fold_buffer(buffer_1.read(cx).remote_id(), cx)
    });
    assert_eq!(
        multi_buffer_editor.update(cx, |editor, cx| editor.display_text(cx)),
        "\n\n\n\n\n4444\n5555\n6666\n\n\n\n\n7777\n8888\n9999\n",
        "After folding the first buffer, its text should not be displayed"
    );

    multi_buffer_editor.update(cx, |editor, cx| {
        editor.fold_buffer(buffer_2.read(cx).remote_id(), cx)
    });

    assert_eq!(
        multi_buffer_editor.update(cx, |editor, cx| editor.display_text(cx)),
        "\n\n\n\n\n\n\n7777\n8888\n9999\n",
        "After folding the second buffer, its text should not be displayed"
    );

    multi_buffer_editor.update(cx, |editor, cx| {
        editor.fold_buffer(buffer_3.read(cx).remote_id(), cx)
    });
    assert_eq!(
        multi_buffer_editor.update(cx, |editor, cx| editor.display_text(cx)),
        "\n\n\n\n\n",
        "After folding the third buffer, its text should not be displayed"
    );

    multi_buffer_editor.update(cx, |editor, cx| {
        editor.unfold_buffer(buffer_2.read(cx).remote_id(), cx)
    });
    assert_eq!(
        multi_buffer_editor.update(cx, |editor, cx| editor.display_text(cx)),
        "\n\n\n\n\n4444\n5555\n6666\n\n\n",
        "After unfolding the second buffer, its text should be displayed"
    );

    multi_buffer_editor.update(cx, |editor, cx| {
        editor.unfold_buffer(buffer_1.read(cx).remote_id(), cx)
    });
    assert_eq!(
        multi_buffer_editor.update(cx, |editor, cx| editor.display_text(cx)),
        "\n\n\n1111\n2222\n3333\n\n\n\n\n4444\n5555\n6666\n\n\n",
        "After unfolding the first buffer, its text should be displayed"
    );

    multi_buffer_editor.update(cx, |editor, cx| {
        editor.unfold_buffer(buffer_3.read(cx).remote_id(), cx)
    });
    assert_eq!(
        multi_buffer_editor.update(cx, |editor, cx| editor.display_text(cx)),
        full_text,
        "After unfolding all buffers, all original text should be displayed"
    );
}

#[gpui::test]
async fn test_multi_buffer_with_single_excerpt_folding(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});

    let sample_text = "aaaa\nbbbb\ncccc\ndddd\neeee\nffff\ngggg\nhhhh\niiii\njjjj".to_string();

    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(
        "/a",
        json!({
            "main.rs": sample_text,
        }),
    )
    .await;
    let project = Project::test(fs, ["/a".as_ref()], cx).await;
    let workspace = cx.add_window(|cx| Workspace::test_new(project.clone(), cx));
    let cx = &mut VisualTestContext::from_window(*workspace.deref(), cx);
    let worktree = project.update(cx, |project, cx| {
        let mut worktrees = project.worktrees(cx).collect::<Vec<_>>();
        assert_eq!(worktrees.len(), 1);
        worktrees.pop().unwrap()
    });
    let worktree_id = worktree.update(cx, |worktree, _| worktree.id());

    let buffer_1 = project
        .update(cx, |project, cx| {
            project.open_buffer((worktree_id, "main.rs"), cx)
        })
        .await
        .unwrap();

    let multi_buffer = cx.new_model(|cx| {
        let mut multi_buffer = MultiBuffer::new(ReadWrite);
        multi_buffer.push_excerpts(
            buffer_1.clone(),
            [ExcerptRange {
                context: Point::new(0, 0)
                    ..Point::new(
                        sample_text.chars().filter(|&c| c == '\n').count() as u32 + 1,
                        0,
                    ),
                primary: None,
            }],
            cx,
        );
        multi_buffer
    });
    let multi_buffer_editor = cx.new_view(|cx| {
        Editor::new(
            EditorMode::Full,
            multi_buffer,
            Some(project.clone()),
            true,
            cx,
        )
    });

    let selection_range = Point::new(1, 0)..Point::new(2, 0);
    multi_buffer_editor.update(cx, |editor, cx| {
        enum TestHighlight {}
        let multi_buffer_snapshot = editor.buffer().read(cx).snapshot(cx);
        let highlight_range = selection_range.clone().to_anchors(&multi_buffer_snapshot);
        editor.highlight_text::<TestHighlight>(
            vec![highlight_range.clone()],
            HighlightStyle::color(Hsla::green()),
            cx,
        );
        editor.change_selections(None, cx, |s| s.select_ranges(Some(highlight_range)));
    });

    let full_text = format!("\n\n\n{sample_text}\n");
    assert_eq!(
        multi_buffer_editor.update(cx, |editor, cx| editor.display_text(cx)),
        full_text,
    );
}

#[gpui::test]
fn test_inline_completion_text(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    // Simple insertion
    {
        let window = cx.add_window(|cx| {
            let buffer = MultiBuffer::build_simple("Hello, world!", cx);
            Editor::new(EditorMode::Full, buffer, None, true, cx)
        });
        let cx = &mut VisualTestContext::from_window(*window, cx);

        window
            .update(cx, |editor, cx| {
                let snapshot = editor.snapshot(cx);
                let edit_range = snapshot.buffer_snapshot.anchor_after(Point::new(0, 6))
                    ..snapshot.buffer_snapshot.anchor_before(Point::new(0, 6));
                let edits = vec![(edit_range, " beautiful".to_string())];

                let InlineCompletionText::Edit { text, highlights } =
                    inline_completion_edit_text(&snapshot, &edits, false, cx)
                else {
                    panic!("Failed to generate inline completion text");
                };

                assert_eq!(text, "Hello, beautiful world!");
                assert_eq!(highlights.len(), 1);
                assert_eq!(highlights[0].0, 6..16);
                assert_eq!(
                    highlights[0].1.background_color,
                    Some(cx.theme().status().created_background)
                );
            })
            .unwrap();
    }

    // Replacement
    {
        let window = cx.add_window(|cx| {
            let buffer = MultiBuffer::build_simple("This is a test.", cx);
            Editor::new(EditorMode::Full, buffer, None, true, cx)
        });
        let cx = &mut VisualTestContext::from_window(*window, cx);

        window
            .update(cx, |editor, cx| {
                let snapshot = editor.snapshot(cx);
                let edits = vec![(
                    snapshot.buffer_snapshot.anchor_after(Point::new(0, 0))
                        ..snapshot.buffer_snapshot.anchor_before(Point::new(0, 4)),
                    "That".to_string(),
                )];

                let InlineCompletionText::Edit { text, highlights } =
                    inline_completion_edit_text(&snapshot, &edits, false, cx)
                else {
                    panic!("Failed to generate inline completion text");
                };

                assert_eq!(text, "That is a test.");
                assert_eq!(highlights.len(), 1);
                assert_eq!(highlights[0].0, 0..4);
                assert_eq!(
                    highlights[0].1.background_color,
                    Some(cx.theme().status().created_background)
                );
            })
            .unwrap();
    }

    // Multiple edits
    {
        let window = cx.add_window(|cx| {
            let buffer = MultiBuffer::build_simple("Hello, world!", cx);
            Editor::new(EditorMode::Full, buffer, None, true, cx)
        });
        let cx = &mut VisualTestContext::from_window(*window, cx);

        window
            .update(cx, |editor, cx| {
                let snapshot = editor.snapshot(cx);
                let edits = vec![
                    (
                        snapshot.buffer_snapshot.anchor_after(Point::new(0, 0))
                            ..snapshot.buffer_snapshot.anchor_before(Point::new(0, 5)),
                        "Greetings".into(),
                    ),
                    (
                        snapshot.buffer_snapshot.anchor_after(Point::new(0, 12))
                            ..snapshot.buffer_snapshot.anchor_before(Point::new(0, 12)),
                        " and universe".into(),
                    ),
                ];

                let InlineCompletionText::Edit { text, highlights } =
                    inline_completion_edit_text(&snapshot, &edits, false, cx)
                else {
                    panic!("Failed to generate inline completion text");
                };

                assert_eq!(text, "Greetings, world and universe!");
                assert_eq!(highlights.len(), 2);
                assert_eq!(highlights[0].0, 0..9);
                assert_eq!(highlights[1].0, 16..29);
                assert_eq!(
                    highlights[0].1.background_color,
                    Some(cx.theme().status().created_background)
                );
                assert_eq!(
                    highlights[1].1.background_color,
                    Some(cx.theme().status().created_background)
                );
            })
            .unwrap();
    }

    // Multiple lines with edits
    {
        let window = cx.add_window(|cx| {
            let buffer =
                MultiBuffer::build_simple("First line\nSecond line\nThird line\nFourth line", cx);
            Editor::new(EditorMode::Full, buffer, None, true, cx)
        });
        let cx = &mut VisualTestContext::from_window(*window, cx);

        window
            .update(cx, |editor, cx| {
                let snapshot = editor.snapshot(cx);
                let edits = vec![
                    (
                        snapshot.buffer_snapshot.anchor_before(Point::new(1, 7))
                            ..snapshot.buffer_snapshot.anchor_before(Point::new(1, 11)),
                        "modified".to_string(),
                    ),
                    (
                        snapshot.buffer_snapshot.anchor_before(Point::new(2, 0))
                            ..snapshot.buffer_snapshot.anchor_before(Point::new(2, 10)),
                        "New third line".to_string(),
                    ),
                    (
                        snapshot.buffer_snapshot.anchor_before(Point::new(3, 6))
                            ..snapshot.buffer_snapshot.anchor_before(Point::new(3, 6)),
                        " updated".to_string(),
                    ),
                ];

                let InlineCompletionText::Edit { text, highlights } =
                    inline_completion_edit_text(&snapshot, &edits, false, cx)
                else {
                    panic!("Failed to generate inline completion text");
                };

                assert_eq!(text, "Second modified\nNew third line\nFourth updated line");
                assert_eq!(highlights.len(), 3);
                assert_eq!(highlights[0].0, 7..15); // "modified"
                assert_eq!(highlights[1].0, 16..30); // "New third line"
                assert_eq!(highlights[2].0, 37..45); // " updated"

                for highlight in &highlights {
                    assert_eq!(
                        highlight.1.background_color,
                        Some(cx.theme().status().created_background)
                    );
                }
            })
            .unwrap();
    }
}

#[gpui::test]
fn test_inline_completion_text_with_deletions(cx: &mut TestAppContext) {
    init_test(cx, |_| {});

    // Deletion
    {
        let window = cx.add_window(|cx| {
            let buffer = MultiBuffer::build_simple("Hello, world!", cx);
            Editor::new(EditorMode::Full, buffer, None, true, cx)
        });
        let cx = &mut VisualTestContext::from_window(*window, cx);

        window
            .update(cx, |editor, cx| {
                let snapshot = editor.snapshot(cx);
                let edit_range = snapshot.buffer_snapshot.anchor_after(Point::new(0, 5))
                    ..snapshot.buffer_snapshot.anchor_before(Point::new(0, 11));
                let edits = vec![(edit_range, "".to_string())];

                let InlineCompletionText::Edit { text, highlights } =
                    inline_completion_edit_text(&snapshot, &edits, true, cx)
                else {
                    panic!("Failed to generate inline completion text");
                };

                assert_eq!(text, "Hello, world!");
                assert_eq!(highlights.len(), 1);
                assert_eq!(highlights[0].0, 5..11);
                assert_eq!(
                    highlights[0].1.background_color,
                    Some(cx.theme().status().deleted_background)
                );
            })
            .unwrap();
    }

    // Insertion
    {
        let window = cx.add_window(|cx| {
            let buffer = MultiBuffer::build_simple("Hello, world!", cx);
            Editor::new(EditorMode::Full, buffer, None, true, cx)
        });
        let cx = &mut VisualTestContext::from_window(*window, cx);

        window
            .update(cx, |editor, cx| {
                let snapshot = editor.snapshot(cx);
                let edit_range = snapshot.buffer_snapshot.anchor_after(Point::new(0, 6))
                    ..snapshot.buffer_snapshot.anchor_before(Point::new(0, 6));
                let edits = vec![(edit_range, " digital".to_string())];

                let InlineCompletionText::Edit { text, highlights } =
                    inline_completion_edit_text(&snapshot, &edits, true, cx)
                else {
                    panic!("Failed to generate inline completion text");
                };

                assert_eq!(text, "Hello, digital world!");
                assert_eq!(highlights.len(), 1);
                assert_eq!(highlights[0].0, 6..14);
                assert_eq!(
                    highlights[0].1.background_color,
                    Some(cx.theme().status().created_background)
                );
            })
            .unwrap();
    }
}

#[gpui::test]
async fn test_rename_with_duplicate_edits(cx: &mut gpui::TestAppContext) {
    init_test(cx, |_| {});
    let mut cx = EditorLspTestContext::new_rust(lsp::ServerCapabilities::default(), cx).await;

    cx.set_state(indoc! {"
        struct Fˇoo {}
    "});

    cx.update_editor(|editor, cx| {
        let highlight_range = Point::new(0, 7)..Point::new(0, 10);
        let highlight_range = highlight_range.to_anchors(&editor.buffer().read(cx).snapshot(cx));
        editor.highlight_background::<DocumentHighlightRead>(
            &[highlight_range],
            |c| c.editor_document_highlight_read_background,
            cx,
        );
    });

    cx.update_editor(|e, cx| e.rename(&Rename, cx))
        .expect("Rename was not started")
        .await
        .expect("Rename failed");
    let mut rename_handler =
        cx.handle_request::<lsp::request::Rename, _, _>(move |url, _, _| async move {
            let edit = lsp::TextEdit {
                range: lsp::Range {
                    start: lsp::Position {
                        line: 0,
                        character: 7,
                    },
                    end: lsp::Position {
                        line: 0,
                        character: 10,
                    },
                },
                new_text: "FooRenamed".to_string(),
            };
            Ok(Some(lsp::WorkspaceEdit::new(
                // Specify the same edit twice
                std::collections::HashMap::from_iter(Some((url, vec![edit.clone(), edit]))),
            )))
        });
    cx.update_editor(|e, cx| e.confirm_rename(&ConfirmRename, cx))
        .expect("Confirm rename was not started")
        .await
        .expect("Confirm rename failed");
    rename_handler.next().await.unwrap();
    cx.run_until_parked();

    // Despite two edits, only one is actually applied as those are identical
    cx.assert_editor_state(indoc! {"
        struct FooRenamedˇ {}
    "});
}

fn empty_range(row: usize, column: usize) -> Range<DisplayPoint> {
    let point = DisplayPoint::new(DisplayRow(row as u32), column as u32);
    point..point
}

fn assert_selection_ranges(marked_text: &str, view: &mut Editor, cx: &mut ViewContext<Editor>) {
    let (text, ranges) = marked_text_ranges(marked_text, true);
    assert_eq!(view.text(cx), text);
    assert_eq!(
        view.selections.ranges(cx),
        ranges,
        "Assert selections are {}",
        marked_text
    );
}

pub fn handle_signature_help_request(
    cx: &mut EditorLspTestContext,
    mocked_response: lsp::SignatureHelp,
) -> impl Future<Output = ()> {
    let mut request =
        cx.handle_request::<lsp::request::SignatureHelpRequest, _, _>(move |_, _, _| {
            let mocked_response = mocked_response.clone();
            async move { Ok(Some(mocked_response)) }
        });

    async move {
        request.next().await;
    }
}

/// Handle completion request passing a marked string specifying where the completion
/// should be triggered from using '|' character, what range should be replaced, and what completions
/// should be returned using '<' and '>' to delimit the range
pub fn handle_completion_request(
    cx: &mut EditorLspTestContext,
    marked_string: &str,
    completions: Vec<&'static str>,
    counter: Arc<AtomicUsize>,
) -> impl Future<Output = ()> {
    let complete_from_marker: TextRangeMarker = '|'.into();
    let replace_range_marker: TextRangeMarker = ('<', '>').into();
    let (_, mut marked_ranges) = marked_text_ranges_by(
        marked_string,
        vec![complete_from_marker.clone(), replace_range_marker.clone()],
    );

    let complete_from_position =
        cx.to_lsp(marked_ranges.remove(&complete_from_marker).unwrap()[0].start);
    let replace_range =
        cx.to_lsp_range(marked_ranges.remove(&replace_range_marker).unwrap()[0].clone());

    let mut request = cx.handle_request::<lsp::request::Completion, _, _>(move |url, params, _| {
        let completions = completions.clone();
        counter.fetch_add(1, atomic::Ordering::Release);
        async move {
            assert_eq!(params.text_document_position.text_document.uri, url.clone());
            assert_eq!(
                params.text_document_position.position,
                complete_from_position
            );
            Ok(Some(lsp::CompletionResponse::Array(
                completions
                    .iter()
                    .map(|completion_text| lsp::CompletionItem {
                        label: completion_text.to_string(),
                        text_edit: Some(lsp::CompletionTextEdit::Edit(lsp::TextEdit {
                            range: replace_range,
                            new_text: completion_text.to_string(),
                        })),
                        ..Default::default()
                    })
                    .collect(),
            )))
        }
    });

    async move {
        request.next().await;
    }
}

fn handle_resolve_completion_request(
    cx: &mut EditorLspTestContext,
    edits: Option<Vec<(&'static str, &'static str)>>,
) -> impl Future<Output = ()> {
    let edits = edits.map(|edits| {
        edits
            .iter()
            .map(|(marked_string, new_text)| {
                let (_, marked_ranges) = marked_text_ranges(marked_string, false);
                let replace_range = cx.to_lsp_range(marked_ranges[0].clone());
                lsp::TextEdit::new(replace_range, new_text.to_string())
            })
            .collect::<Vec<_>>()
    });

    let mut request =
        cx.handle_request::<lsp::request::ResolveCompletionItem, _, _>(move |_, _, _| {
            let edits = edits.clone();
            async move {
                Ok(lsp::CompletionItem {
                    additional_text_edits: edits,
                    ..Default::default()
                })
            }
        });

    async move {
        request.next().await;
    }
}

pub(crate) fn update_test_language_settings(
    cx: &mut TestAppContext,
    f: impl Fn(&mut AllLanguageSettingsContent),
) {
    cx.update(|cx| {
        SettingsStore::update_global(cx, |store, cx| {
            store.update_user_settings::<AllLanguageSettings>(cx, f);
        });
    });
}

pub(crate) fn update_test_project_settings(
    cx: &mut TestAppContext,
    f: impl Fn(&mut ProjectSettings),
) {
    cx.update(|cx| {
        SettingsStore::update_global(cx, |store, cx| {
            store.update_user_settings::<ProjectSettings>(cx, f);
        });
    });
}

pub(crate) fn init_test(cx: &mut TestAppContext, f: fn(&mut AllLanguageSettingsContent)) {
    cx.update(|cx| {
        assets::Assets.load_test_fonts(cx);
        let store = SettingsStore::test(cx);
        cx.set_global(store);
        theme::init(theme::LoadThemes::JustBase, cx);
        release_channel::init(SemanticVersion::default(), cx);
        client::init_settings(cx);
        language::init(cx);
        Project::init_settings(cx);
        workspace::init_settings(cx);
        crate::init(cx);
    });

    update_test_language_settings(cx, f);
}

#[track_caller]
fn assert_hunk_revert(
    not_reverted_text_with_selections: &str,
    expected_not_reverted_hunk_statuses: Vec<DiffHunkStatus>,
    expected_reverted_text_with_selections: &str,
    base_text: &str,
    cx: &mut EditorLspTestContext,
) {
    cx.set_state(not_reverted_text_with_selections);
    cx.set_diff_base(base_text);
    cx.executor().run_until_parked();

    let reverted_hunk_statuses = cx.update_editor(|editor, cx| {
        let snapshot = editor.snapshot(cx);
        let reverted_hunk_statuses = snapshot
            .diff_map
            .diff_hunks_in_range(0..snapshot.buffer_snapshot.len(), &snapshot.buffer_snapshot)
            .map(|hunk| hunk_status(&hunk))
            .collect::<Vec<_>>();

        editor.revert_selected_hunks(&RevertSelectedHunks, cx);
        reverted_hunk_statuses
    });
    cx.executor().run_until_parked();
    cx.assert_editor_state(expected_reverted_text_with_selections);
    assert_eq!(reverted_hunk_statuses, expected_not_reverted_hunk_statuses);
}
