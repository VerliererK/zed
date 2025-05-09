use std::cell::RefCell;
use std::{cmp::Reverse, ops::Range, rc::Rc};

use fuzzy::{StringMatch, StringMatchCandidate};
use gpui::{
    div, px, uniform_list, AnyElement, BackgroundExecutor, Div, FontWeight, ListSizingBehavior,
    Model, ScrollStrategy, SharedString, StrikethroughStyle, StyledText, UniformListScrollHandle,
    ViewContext, WeakView,
};
use language::Buffer;
use language::{CodeLabel, Documentation};
use lsp::LanguageServerId;
use multi_buffer::{Anchor, ExcerptId};
use ordered_float::OrderedFloat;
use project::{CodeAction, Completion, TaskSourceKind};
use task::ResolvedTask;
use ui::{prelude::*, Color, IntoElement, ListItem, Pixels, Popover, Styled};
use util::ResultExt;
use workspace::Workspace;

use crate::{
    actions::{ConfirmCodeAction, ConfirmCompletion},
    display_map::DisplayPoint,
    render_parsed_markdown, split_words, styled_runs_for_code_label, CodeActionProvider,
    CompletionId, CompletionProvider, DisplayRow, Editor, EditorStyle, ResolvedTasks,
};
use crate::{AcceptInlineCompletion, InlineCompletionMenuHint, InlineCompletionText};

pub const MAX_COMPLETIONS_ASIDE_WIDTH: Pixels = px(500.);

pub enum CodeContextMenu {
    Completions(CompletionsMenu),
    CodeActions(CodeActionsMenu),
}

impl CodeContextMenu {
    pub fn select_first(
        &mut self,
        provider: Option<&dyn CompletionProvider>,
        cx: &mut ViewContext<Editor>,
    ) -> bool {
        if self.visible() {
            match self {
                CodeContextMenu::Completions(menu) => menu.select_first(provider, cx),
                CodeContextMenu::CodeActions(menu) => menu.select_first(cx),
            }
            true
        } else {
            false
        }
    }

    pub fn select_prev(
        &mut self,
        provider: Option<&dyn CompletionProvider>,
        cx: &mut ViewContext<Editor>,
    ) -> bool {
        if self.visible() {
            match self {
                CodeContextMenu::Completions(menu) => menu.select_prev(provider, cx),
                CodeContextMenu::CodeActions(menu) => menu.select_prev(cx),
            }
            true
        } else {
            false
        }
    }

    pub fn select_next(
        &mut self,
        provider: Option<&dyn CompletionProvider>,
        cx: &mut ViewContext<Editor>,
    ) -> bool {
        if self.visible() {
            match self {
                CodeContextMenu::Completions(menu) => menu.select_next(provider, cx),
                CodeContextMenu::CodeActions(menu) => menu.select_next(cx),
            }
            true
        } else {
            false
        }
    }

    pub fn select_last(
        &mut self,
        provider: Option<&dyn CompletionProvider>,
        cx: &mut ViewContext<Editor>,
    ) -> bool {
        if self.visible() {
            match self {
                CodeContextMenu::Completions(menu) => menu.select_last(provider, cx),
                CodeContextMenu::CodeActions(menu) => menu.select_last(cx),
            }
            true
        } else {
            false
        }
    }

    pub fn visible(&self) -> bool {
        match self {
            CodeContextMenu::Completions(menu) => menu.visible(),
            CodeContextMenu::CodeActions(menu) => menu.visible(),
        }
    }

    pub fn origin(&self, cursor_position: DisplayPoint) -> ContextMenuOrigin {
        match self {
            CodeContextMenu::Completions(menu) => menu.origin(cursor_position),
            CodeContextMenu::CodeActions(menu) => menu.origin(cursor_position),
        }
    }

    pub fn render(
        &self,
        style: &EditorStyle,
        max_height_in_lines: u32,
        cx: &mut ViewContext<Editor>,
    ) -> AnyElement {
        match self {
            CodeContextMenu::Completions(menu) => menu.render(style, max_height_in_lines, cx),
            CodeContextMenu::CodeActions(menu) => menu.render(style, max_height_in_lines, cx),
        }
    }

    pub fn render_aside(
        &self,
        style: &EditorStyle,
        max_height: Pixels,
        workspace: Option<WeakView<Workspace>>,
        cx: &mut ViewContext<Editor>,
    ) -> Option<AnyElement> {
        match self {
            CodeContextMenu::Completions(menu) => {
                menu.render_aside(style, max_height, workspace, cx)
            }
            CodeContextMenu::CodeActions(_) => None,
        }
    }
}

pub enum ContextMenuOrigin {
    EditorPoint(DisplayPoint),
    GutterIndicator(DisplayRow),
}

#[derive(Clone, Debug)]
pub struct CompletionsMenu {
    pub id: CompletionId,
    sort_completions: bool,
    pub initial_position: Anchor,
    pub buffer: Model<Buffer>,
    pub completions: Rc<RefCell<Box<[Completion]>>>,
    match_candidates: Rc<[StringMatchCandidate]>,
    pub entries: Rc<[CompletionEntry]>,
    pub selected_item: usize,
    scroll_handle: UniformListScrollHandle,
    resolve_completions: bool,
    show_completion_documentation: bool,
}

#[derive(Clone, Debug)]
pub(crate) enum CompletionEntry {
    Match(StringMatch),
    InlineCompletionHint(InlineCompletionMenuHint),
}

impl CompletionsMenu {
    pub fn new(
        id: CompletionId,
        sort_completions: bool,
        show_completion_documentation: bool,
        initial_position: Anchor,
        buffer: Model<Buffer>,
        completions: Box<[Completion]>,
    ) -> Self {
        let match_candidates = completions
            .iter()
            .enumerate()
            .map(|(id, completion)| StringMatchCandidate::new(id, &completion.label.filter_text()))
            .collect();

        Self {
            id,
            sort_completions,
            initial_position,
            buffer,
            show_completion_documentation,
            completions: RefCell::new(completions).into(),
            match_candidates,
            entries: Vec::new().into(),
            selected_item: 0,
            scroll_handle: UniformListScrollHandle::new(),
            resolve_completions: true,
        }
    }

    pub fn new_snippet_choices(
        id: CompletionId,
        sort_completions: bool,
        choices: &Vec<String>,
        selection: Range<Anchor>,
        buffer: Model<Buffer>,
    ) -> Self {
        let completions = choices
            .iter()
            .map(|choice| Completion {
                old_range: selection.start.text_anchor..selection.end.text_anchor,
                new_text: choice.to_string(),
                label: CodeLabel {
                    text: choice.to_string(),
                    runs: Default::default(),
                    filter_range: Default::default(),
                },
                server_id: LanguageServerId(usize::MAX),
                documentation: None,
                lsp_completion: Default::default(),
                confirm: None,
                resolved: true,
            })
            .collect();

        let match_candidates = choices
            .iter()
            .enumerate()
            .map(|(id, completion)| StringMatchCandidate::new(id, &completion))
            .collect();
        let entries = choices
            .iter()
            .enumerate()
            .map(|(id, completion)| {
                CompletionEntry::Match(StringMatch {
                    candidate_id: id,
                    score: 1.,
                    positions: vec![],
                    string: completion.clone(),
                })
            })
            .collect();
        Self {
            id,
            sort_completions,
            initial_position: selection.start,
            buffer,
            completions: RefCell::new(completions).into(),
            match_candidates,
            entries,
            selected_item: 0,
            scroll_handle: UniformListScrollHandle::new(),
            resolve_completions: false,
            show_completion_documentation: false,
        }
    }

    fn select_first(
        &mut self,
        provider: Option<&dyn CompletionProvider>,
        cx: &mut ViewContext<Editor>,
    ) {
        self.selected_item = 0;
        self.scroll_handle
            .scroll_to_item(self.selected_item, ScrollStrategy::Top);
        self.resolve_selected_completion(provider, cx);
        cx.notify();
    }

    fn select_prev(
        &mut self,
        provider: Option<&dyn CompletionProvider>,
        cx: &mut ViewContext<Editor>,
    ) {
        if self.selected_item > 0 {
            self.selected_item -= 1;
        } else {
            self.selected_item = self.entries.len() - 1;
        }
        self.scroll_handle
            .scroll_to_item(self.selected_item, ScrollStrategy::Top);
        self.resolve_selected_completion(provider, cx);
        cx.notify();
    }

    fn select_next(
        &mut self,
        provider: Option<&dyn CompletionProvider>,
        cx: &mut ViewContext<Editor>,
    ) {
        if self.selected_item + 1 < self.entries.len() {
            self.selected_item += 1;
        } else {
            self.selected_item = 0;
        }
        self.scroll_handle
            .scroll_to_item(self.selected_item, ScrollStrategy::Top);
        self.resolve_selected_completion(provider, cx);
        cx.notify();
    }

    fn select_last(
        &mut self,
        provider: Option<&dyn CompletionProvider>,
        cx: &mut ViewContext<Editor>,
    ) {
        self.selected_item = self.entries.len() - 1;
        self.scroll_handle
            .scroll_to_item(self.selected_item, ScrollStrategy::Top);
        self.resolve_selected_completion(provider, cx);
        cx.notify();
    }

    pub fn show_inline_completion_hint(&mut self, hint: InlineCompletionMenuHint) {
        let hint = CompletionEntry::InlineCompletionHint(hint);

        self.entries = match self.entries.first() {
            Some(CompletionEntry::InlineCompletionHint { .. }) => {
                let mut entries = Vec::from(&*self.entries);
                entries[0] = hint;
                entries
            }
            _ => {
                let mut entries = Vec::with_capacity(self.entries.len() + 1);
                entries.push(hint);
                entries.extend_from_slice(&self.entries);
                entries
            }
        }
        .into();
        self.selected_item = 0;
    }

    pub fn resolve_selected_completion(
        &mut self,
        provider: Option<&dyn CompletionProvider>,
        cx: &mut ViewContext<Editor>,
    ) {
        if !self.resolve_completions {
            return;
        }
        let Some(provider) = provider else {
            return;
        };

        match &self.entries[self.selected_item] {
            CompletionEntry::Match(entry) => {
                let completion_index = entry.candidate_id;
                let resolve_task = provider.resolve_completions(
                    self.buffer.clone(),
                    vec![completion_index],
                    self.completions.clone(),
                    cx,
                );

                cx.spawn(move |editor, mut cx| async move {
                    if let Some(true) = resolve_task.await.log_err() {
                        editor.update(&mut cx, |_, cx| cx.notify()).ok();
                    }
                })
                .detach();
            }
            CompletionEntry::InlineCompletionHint { .. } => {}
        }
    }

    pub fn visible(&self) -> bool {
        !self.entries.is_empty()
    }

    fn origin(&self, cursor_position: DisplayPoint) -> ContextMenuOrigin {
        ContextMenuOrigin::EditorPoint(cursor_position)
    }

    fn render(
        &self,
        style: &EditorStyle,
        max_height_in_lines: u32,
        cx: &mut ViewContext<Editor>,
    ) -> AnyElement {
        let completions = self.completions.borrow_mut();
        let show_completion_documentation = self.show_completion_documentation;
        let widest_completion_ix = self
            .entries
            .iter()
            .enumerate()
            .max_by_key(|(_, mat)| match mat {
                CompletionEntry::Match(mat) => {
                    let completion = &completions[mat.candidate_id];
                    let documentation = &completion.documentation;

                    let mut len = completion.label.text.chars().count();
                    if let Some(Documentation::SingleLine(text)) = documentation {
                        if show_completion_documentation {
                            len += text.chars().count();
                        }
                    }

                    len
                }
                CompletionEntry::InlineCompletionHint(InlineCompletionMenuHint {
                    provider_name,
                    ..
                }) => provider_name.len(),
            })
            .map(|(ix, _)| ix);
        drop(completions);

        let selected_item = self.selected_item;
        let completions = self.completions.clone();
        let matches = self.entries.clone();
        let style = style.clone();
        let list = uniform_list(
            cx.view().clone(),
            "completions",
            matches.len(),
            move |_editor, range, cx| {
                let start_ix = range.start;
                let completions_guard = completions.borrow_mut();

                matches[range]
                    .iter()
                    .enumerate()
                    .map(|(ix, mat)| {
                        let item_ix = start_ix + ix;
                        match mat {
                            CompletionEntry::Match(mat) => {
                                let candidate_id = mat.candidate_id;
                                let completion = &completions_guard[candidate_id];

                                let documentation = if show_completion_documentation {
                                    &completion.documentation
                                } else {
                                    &None
                                };

                                let filter_start = completion.label.filter_range.start;
                                let highlights = gpui::combine_highlights(
                                    mat.ranges().map(|range| {
                                        (
                                            filter_start + range.start..filter_start + range.end,
                                            FontWeight::BOLD.into(),
                                        )
                                    }),
                                    styled_runs_for_code_label(&completion.label, &style.syntax)
                                        .map(|(range, mut highlight)| {
                                            // Ignore font weight for syntax highlighting, as we'll use it
                                            // for fuzzy matches.
                                            highlight.font_weight = None;

                                            if completion.lsp_completion.deprecated.unwrap_or(false)
                                            {
                                                highlight.strikethrough =
                                                    Some(StrikethroughStyle {
                                                        thickness: 1.0.into(),
                                                        ..Default::default()
                                                    });
                                                highlight.color =
                                                    Some(cx.theme().colors().text_muted);
                                            }

                                            (range, highlight)
                                        }),
                                );

                                let completion_label =
                                    StyledText::new(completion.label.text.clone())
                                        .with_highlights(&style.text, highlights);
                                let documentation_label =
                                    if let Some(Documentation::SingleLine(text)) = documentation {
                                        if text.trim().is_empty() {
                                            None
                                        } else {
                                            Some(
                                                Label::new(text.clone())
                                                    .ml_4()
                                                    .size(LabelSize::Small)
                                                    .color(Color::Muted),
                                            )
                                        }
                                    } else {
                                        None
                                    };

                                let color_swatch = completion
                                    .color()
                                    .map(|color| div().size_4().bg(color).rounded_sm());

                                div().min_w(px(220.)).max_w(px(540.)).child(
                                    ListItem::new(mat.candidate_id)
                                        .inset(true)
                                        .toggle_state(item_ix == selected_item)
                                        .on_click(cx.listener(move |editor, _event, cx| {
                                            cx.stop_propagation();
                                            if let Some(task) = editor.confirm_completion(
                                                &ConfirmCompletion {
                                                    item_ix: Some(item_ix),
                                                },
                                                cx,
                                            ) {
                                                task.detach_and_log_err(cx)
                                            }
                                        }))
                                        .start_slot::<Div>(color_swatch)
                                        .child(h_flex().overflow_hidden().child(completion_label))
                                        .end_slot::<Label>(documentation_label),
                                )
                            }
                            CompletionEntry::InlineCompletionHint(InlineCompletionMenuHint {
                                provider_name,
                                ..
                            }) => div().min_w(px(250.)).max_w(px(500.)).child(
                                ListItem::new("inline-completion")
                                    .inset(true)
                                    .toggle_state(item_ix == selected_item)
                                    .start_slot(Icon::new(IconName::ZedPredict))
                                    .child(
                                        StyledText::new(format!(
                                            "{} Completion",
                                            SharedString::new_static(provider_name)
                                        ))
                                        .with_highlights(&style.text, None),
                                    )
                                    .on_click(cx.listener(move |editor, _event, cx| {
                                        cx.stop_propagation();
                                        editor.accept_inline_completion(
                                            &AcceptInlineCompletion {},
                                            cx,
                                        );
                                    })),
                            ),
                        }
                    })
                    .collect()
            },
        )
        .occlude()
        .max_h(max_height_in_lines as f32 * cx.line_height())
        .track_scroll(self.scroll_handle.clone())
        .with_width_from_item(widest_completion_ix)
        .with_sizing_behavior(ListSizingBehavior::Infer);

        Popover::new().child(list).into_any_element()
    }

    fn render_aside(
        &self,
        style: &EditorStyle,
        max_height: Pixels,
        workspace: Option<WeakView<Workspace>>,
        cx: &mut ViewContext<Editor>,
    ) -> Option<AnyElement> {
        if !self.show_completion_documentation {
            return None;
        }

        let multiline_docs = match &self.entries[self.selected_item] {
            CompletionEntry::Match(mat) => {
                match self.completions.borrow_mut()[mat.candidate_id]
                    .documentation
                    .as_ref()?
                {
                    Documentation::MultiLinePlainText(text) => {
                        div().child(SharedString::from(text.clone()))
                    }
                    Documentation::MultiLineMarkdown(parsed) if !parsed.text.is_empty() => div()
                        .child(render_parsed_markdown(
                            "completions_markdown",
                            parsed,
                            &style,
                            workspace,
                            cx,
                        )),
                    Documentation::MultiLineMarkdown(_) => return None,
                    Documentation::SingleLine(_) => return None,
                    Documentation::Undocumented => return None,
                }
            }
            CompletionEntry::InlineCompletionHint(hint) => match &hint.text {
                InlineCompletionText::Edit { text, highlights } => div()
                    .my_1()
                    .rounded(px(6.))
                    .bg(cx.theme().colors().editor_background)
                    .border_1()
                    .border_color(cx.theme().colors().border_variant)
                    .child(
                        gpui::StyledText::new(text.clone())
                            .with_highlights(&style.text, highlights.clone()),
                    ),
                InlineCompletionText::Move(text) => div().child(text.clone()),
            },
        };

        Some(
            Popover::new()
                .child(
                    multiline_docs
                        .id("multiline_docs")
                        .max_h(max_height)
                        .px_0p5()
                        .min_w(px(260.))
                        .max_w(MAX_COMPLETIONS_ASIDE_WIDTH)
                        .overflow_y_scroll()
                        .occlude(),
                )
                .into_any_element(),
        )
    }

    pub async fn filter(&mut self, query: Option<&str>, executor: BackgroundExecutor) {
        let mut matches = if let Some(query) = query {
            fuzzy::match_strings(
                &self.match_candidates,
                query,
                query.chars().any(|c| c.is_uppercase()),
                100,
                &Default::default(),
                executor,
            )
            .await
        } else {
            self.match_candidates
                .iter()
                .enumerate()
                .map(|(candidate_id, candidate)| StringMatch {
                    candidate_id,
                    score: Default::default(),
                    positions: Default::default(),
                    string: candidate.string.clone(),
                })
                .collect()
        };

        // Remove all candidates where the query's start does not match the start of any word in the candidate
        if let Some(query) = query {
            if let Some(query_start) = query.chars().next() {
                matches.retain(|string_match| {
                    split_words(&string_match.string).any(|word| {
                        // Check that the first codepoint of the word as lowercase matches the first
                        // codepoint of the query as lowercase
                        word.chars()
                            .flat_map(|codepoint| codepoint.to_lowercase())
                            .zip(query_start.to_lowercase())
                            .all(|(word_cp, query_cp)| word_cp == query_cp)
                    })
                });
            }
        }

        let completions = self.completions.borrow_mut();
        if self.sort_completions {
            matches.sort_unstable_by_key(|mat| {
                // We do want to strike a balance here between what the language server tells us
                // to sort by (the sort_text) and what are "obvious" good matches (i.e. when you type
                // `Creat` and there is a local variable called `CreateComponent`).
                // So what we do is: we bucket all matches into two buckets
                // - Strong matches
                // - Weak matches
                // Strong matches are the ones with a high fuzzy-matcher score (the "obvious" matches)
                // and the Weak matches are the rest.
                //
                // For the strong matches, we sort by our fuzzy-finder score first and for the weak
                // matches, we prefer language-server sort_text first.
                //
                // The thinking behind that: we want to show strong matches first in order of relevance(fuzzy score).
                // Rest of the matches(weak) can be sorted as language-server expects.

                #[derive(PartialEq, Eq, PartialOrd, Ord)]
                enum MatchScore<'a> {
                    Strong {
                        score: Reverse<OrderedFloat<f64>>,
                        sort_text: Option<&'a str>,
                        sort_key: (usize, &'a str),
                    },
                    Weak {
                        sort_text: Option<&'a str>,
                        score: Reverse<OrderedFloat<f64>>,
                        sort_key: (usize, &'a str),
                    },
                }

                let completion = &completions[mat.candidate_id];
                let sort_key = completion.sort_key();
                let sort_text = completion.lsp_completion.sort_text.as_deref();
                let score = Reverse(OrderedFloat(mat.score));

                if mat.score >= 0.2 {
                    MatchScore::Strong {
                        score,
                        sort_text,
                        sort_key,
                    }
                } else {
                    MatchScore::Weak {
                        sort_text,
                        score,
                        sort_key,
                    }
                }
            });
        }
        drop(completions);

        let mut new_entries: Vec<_> = matches.into_iter().map(CompletionEntry::Match).collect();
        if let Some(CompletionEntry::InlineCompletionHint(hint)) = self.entries.first() {
            new_entries.insert(0, CompletionEntry::InlineCompletionHint(hint.clone()));
        }

        self.entries = new_entries.into();
        self.selected_item = 0;
    }
}

#[derive(Clone)]
pub struct AvailableCodeAction {
    pub excerpt_id: ExcerptId,
    pub action: CodeAction,
    pub provider: Rc<dyn CodeActionProvider>,
}

#[derive(Clone)]
pub struct CodeActionContents {
    pub tasks: Option<Rc<ResolvedTasks>>,
    pub actions: Option<Rc<[AvailableCodeAction]>>,
}

impl CodeActionContents {
    fn len(&self) -> usize {
        match (&self.tasks, &self.actions) {
            (Some(tasks), Some(actions)) => actions.len() + tasks.templates.len(),
            (Some(tasks), None) => tasks.templates.len(),
            (None, Some(actions)) => actions.len(),
            (None, None) => 0,
        }
    }

    fn is_empty(&self) -> bool {
        match (&self.tasks, &self.actions) {
            (Some(tasks), Some(actions)) => actions.is_empty() && tasks.templates.is_empty(),
            (Some(tasks), None) => tasks.templates.is_empty(),
            (None, Some(actions)) => actions.is_empty(),
            (None, None) => true,
        }
    }

    fn iter(&self) -> impl Iterator<Item = CodeActionsItem> + '_ {
        self.tasks
            .iter()
            .flat_map(|tasks| {
                tasks
                    .templates
                    .iter()
                    .map(|(kind, task)| CodeActionsItem::Task(kind.clone(), task.clone()))
            })
            .chain(self.actions.iter().flat_map(|actions| {
                actions.iter().map(|available| CodeActionsItem::CodeAction {
                    excerpt_id: available.excerpt_id,
                    action: available.action.clone(),
                    provider: available.provider.clone(),
                })
            }))
    }

    pub fn get(&self, index: usize) -> Option<CodeActionsItem> {
        match (&self.tasks, &self.actions) {
            (Some(tasks), Some(actions)) => {
                if index < tasks.templates.len() {
                    tasks
                        .templates
                        .get(index)
                        .cloned()
                        .map(|(kind, task)| CodeActionsItem::Task(kind, task))
                } else {
                    actions.get(index - tasks.templates.len()).map(|available| {
                        CodeActionsItem::CodeAction {
                            excerpt_id: available.excerpt_id,
                            action: available.action.clone(),
                            provider: available.provider.clone(),
                        }
                    })
                }
            }
            (Some(tasks), None) => tasks
                .templates
                .get(index)
                .cloned()
                .map(|(kind, task)| CodeActionsItem::Task(kind, task)),
            (None, Some(actions)) => {
                actions
                    .get(index)
                    .map(|available| CodeActionsItem::CodeAction {
                        excerpt_id: available.excerpt_id,
                        action: available.action.clone(),
                        provider: available.provider.clone(),
                    })
            }
            (None, None) => None,
        }
    }
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
pub enum CodeActionsItem {
    Task(TaskSourceKind, ResolvedTask),
    CodeAction {
        excerpt_id: ExcerptId,
        action: CodeAction,
        provider: Rc<dyn CodeActionProvider>,
    },
}

impl CodeActionsItem {
    fn as_task(&self) -> Option<&ResolvedTask> {
        let Self::Task(_, task) = self else {
            return None;
        };
        Some(task)
    }

    fn as_code_action(&self) -> Option<&CodeAction> {
        let Self::CodeAction { action, .. } = self else {
            return None;
        };
        Some(action)
    }

    pub fn label(&self) -> String {
        match self {
            Self::CodeAction { action, .. } => action.lsp_action.title.clone(),
            Self::Task(_, task) => task.resolved_label.clone(),
        }
    }
}

pub struct CodeActionsMenu {
    pub actions: CodeActionContents,
    pub buffer: Model<Buffer>,
    pub selected_item: usize,
    pub scroll_handle: UniformListScrollHandle,
    pub deployed_from_indicator: Option<DisplayRow>,
}

impl CodeActionsMenu {
    fn select_first(&mut self, cx: &mut ViewContext<Editor>) {
        self.selected_item = 0;
        self.scroll_handle
            .scroll_to_item(self.selected_item, ScrollStrategy::Top);
        cx.notify()
    }

    fn select_prev(&mut self, cx: &mut ViewContext<Editor>) {
        if self.selected_item > 0 {
            self.selected_item -= 1;
        } else {
            self.selected_item = self.actions.len() - 1;
        }
        self.scroll_handle
            .scroll_to_item(self.selected_item, ScrollStrategy::Top);
        cx.notify();
    }

    fn select_next(&mut self, cx: &mut ViewContext<Editor>) {
        if self.selected_item + 1 < self.actions.len() {
            self.selected_item += 1;
        } else {
            self.selected_item = 0;
        }
        self.scroll_handle
            .scroll_to_item(self.selected_item, ScrollStrategy::Top);
        cx.notify();
    }

    fn select_last(&mut self, cx: &mut ViewContext<Editor>) {
        self.selected_item = self.actions.len() - 1;
        self.scroll_handle
            .scroll_to_item(self.selected_item, ScrollStrategy::Top);
        cx.notify()
    }

    fn visible(&self) -> bool {
        !self.actions.is_empty()
    }

    fn origin(&self, cursor_position: DisplayPoint) -> ContextMenuOrigin {
        if let Some(row) = self.deployed_from_indicator {
            ContextMenuOrigin::GutterIndicator(row)
        } else {
            ContextMenuOrigin::EditorPoint(cursor_position)
        }
    }

    fn render(
        &self,
        _style: &EditorStyle,
        max_height_in_lines: u32,
        cx: &mut ViewContext<Editor>,
    ) -> AnyElement {
        let actions = self.actions.clone();
        let selected_item = self.selected_item;
        let list = uniform_list(
            cx.view().clone(),
            "code_actions_menu",
            self.actions.len(),
            move |_this, range, cx| {
                actions
                    .iter()
                    .skip(range.start)
                    .take(range.end - range.start)
                    .enumerate()
                    .map(|(ix, action)| {
                        let item_ix = range.start + ix;
                        let selected = item_ix == selected_item;
                        let colors = cx.theme().colors();
                        div().min_w(px(220.)).max_w(px(540.)).child(
                            ListItem::new(item_ix)
                                .inset(true)
                                .toggle_state(selected)
                                .when_some(action.as_code_action(), |this, action| {
                                    this.on_click(cx.listener(move |editor, _, cx| {
                                        cx.stop_propagation();
                                        if let Some(task) = editor.confirm_code_action(
                                            &ConfirmCodeAction {
                                                item_ix: Some(item_ix),
                                            },
                                            cx,
                                        ) {
                                            task.detach_and_log_err(cx)
                                        }
                                    }))
                                    .child(
                                        h_flex()
                                            .overflow_hidden()
                                            .child(
                                                // TASK: It would be good to make lsp_action.title a SharedString to avoid allocating here.
                                                action.lsp_action.title.replace("\n", ""),
                                            )
                                            .when(selected, |this| {
                                                this.text_color(colors.text_accent)
                                            }),
                                    )
                                })
                                .when_some(action.as_task(), |this, task| {
                                    this.on_click(cx.listener(move |editor, _, cx| {
                                        cx.stop_propagation();
                                        if let Some(task) = editor.confirm_code_action(
                                            &ConfirmCodeAction {
                                                item_ix: Some(item_ix),
                                            },
                                            cx,
                                        ) {
                                            task.detach_and_log_err(cx)
                                        }
                                    }))
                                    .child(
                                        h_flex()
                                            .overflow_hidden()
                                            .child(task.resolved_label.replace("\n", ""))
                                            .when(selected, |this| {
                                                this.text_color(colors.text_accent)
                                            }),
                                    )
                                }),
                        )
                    })
                    .collect()
            },
        )
        .occlude()
        .max_h(max_height_in_lines as f32 * cx.line_height())
        .track_scroll(self.scroll_handle.clone())
        .with_width_from_item(
            self.actions
                .iter()
                .enumerate()
                .max_by_key(|(_, action)| match action {
                    CodeActionsItem::Task(_, task) => task.resolved_label.chars().count(),
                    CodeActionsItem::CodeAction { action, .. } => {
                        action.lsp_action.title.chars().count()
                    }
                })
                .map(|(ix, _)| ix),
        )
        .with_sizing_behavior(ListSizingBehavior::Infer);

        Popover::new().child(list).into_any_element()
    }
}
