#![allow(missing_docs)]

use gpui::{relative, AnyElement, FontWeight, StyleRefinement, Styled, UnderlineStyle};
use settings::Settings;
use smallvec::SmallVec;
use theme::ThemeSettings;

use crate::prelude::*;

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy, Default)]
pub enum LabelSize {
    #[default]
    Default,
    Large,
    Small,
    XSmall,
}

#[derive(Default, PartialEq, Copy, Clone)]
pub enum LineHeightStyle {
    #[default]
    TextLabel,
    /// Sets the line height to 1.
    UiLabel,
}

/// A common set of traits all labels must implement.
pub trait LabelCommon {
    /// Sets the size of the label using a [`LabelSize`].
    fn size(self, size: LabelSize) -> Self;

    /// Sets the font weight of the label.
    fn weight(self, weight: FontWeight) -> Self;

    /// Sets the line height style of the label using a [`LineHeightStyle`].
    fn line_height_style(self, line_height_style: LineHeightStyle) -> Self;

    /// Sets the color of the label using a [`Color`].
    fn color(self, color: Color) -> Self;

    /// Sets the strikethrough property of the label.
    fn strikethrough(self, strikethrough: bool) -> Self;

    /// Sets the italic property of the label.
    fn italic(self, italic: bool) -> Self;

    /// Sets the underline property of the label
    fn underline(self, underline: bool) -> Self;

    /// Sets the alpha property of the label, overwriting the alpha value of the color.
    fn alpha(self, alpha: f32) -> Self;

    /// Truncates overflowing text with an ellipsis (`…`) if needed.
    fn text_ellipsis(self) -> Self;

    /// Sets the label to render as a single line.
    fn single_line(self) -> Self;
}

#[derive(IntoElement)]
pub struct LabelLike {
    pub(super) base: Div,
    size: LabelSize,
    weight: Option<FontWeight>,
    line_height_style: LineHeightStyle,
    pub(crate) color: Color,
    strikethrough: bool,
    italic: bool,
    children: SmallVec<[AnyElement; 2]>,
    alpha: Option<f32>,
    underline: bool,
    single_line: bool,
    text_ellipsis: bool,
}

impl Default for LabelLike {
    fn default() -> Self {
        Self::new()
    }
}

impl LabelLike {
    pub fn new() -> Self {
        Self {
            base: div(),
            size: LabelSize::Default,
            weight: None,
            line_height_style: LineHeightStyle::default(),
            color: Color::Default,
            strikethrough: false,
            italic: false,
            children: SmallVec::new(),
            alpha: None,
            underline: false,
            single_line: false,
            text_ellipsis: false,
        }
    }
}

// Style methods.
impl LabelLike {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }

    gpui::margin_style_methods!({
        visibility: pub
    });
}

impl LabelCommon for LabelLike {
    fn size(mut self, size: LabelSize) -> Self {
        self.size = size;
        self
    }

    fn weight(mut self, weight: FontWeight) -> Self {
        self.weight = Some(weight);
        self
    }

    fn line_height_style(mut self, line_height_style: LineHeightStyle) -> Self {
        self.line_height_style = line_height_style;
        self
    }

    fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    fn strikethrough(mut self, strikethrough: bool) -> Self {
        self.strikethrough = strikethrough;
        self
    }

    fn italic(mut self, italic: bool) -> Self {
        self.italic = italic;
        self
    }

    fn underline(mut self, underline: bool) -> Self {
        self.underline = underline;
        self
    }

    fn alpha(mut self, alpha: f32) -> Self {
        self.alpha = Some(alpha);
        self
    }

    fn text_ellipsis(mut self) -> Self {
        self.text_ellipsis = true;
        self
    }

    fn single_line(mut self) -> Self {
        self.single_line = true;
        self
    }
}

impl ParentElement for LabelLike {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements)
    }
}

impl RenderOnce for LabelLike {
    fn render(self, cx: &mut WindowContext) -> impl IntoElement {
        let settings = ThemeSettings::get_global(cx);

        let mut color = self.color.color(cx);
        if let Some(alpha) = self.alpha {
            color.fade_out(1.0 - alpha);
        }

        self.base
            .map(|this| match self.size {
                LabelSize::Large => this.text_ui_lg(cx),
                LabelSize::Default => this.text_ui(cx),
                LabelSize::Small => this.text_ui_sm(cx),
                LabelSize::XSmall => this.text_ui_xs(cx),
            })
            .when(self.line_height_style == LineHeightStyle::UiLabel, |this| {
                this.line_height(relative(1.))
            })
            .when(self.italic, |this| this.italic())
            .when(self.underline, |mut this| {
                this.text_style()
                    .get_or_insert_with(Default::default)
                    .underline = Some(UnderlineStyle {
                    thickness: px(1.),
                    color: None,
                    wavy: false,
                });
                this
            })
            .when(self.strikethrough, |this| this.line_through())
            .when(self.single_line, |this| this.whitespace_nowrap())
            .when(self.text_ellipsis, |this| {
                this.overflow_x_hidden().text_ellipsis()
            })
            .text_color(color)
            .font_weight(self.weight.unwrap_or(settings.ui_font.weight))
            .children(self.children)
    }
}
