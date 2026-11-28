//! Flexbox layout for widgets: rows, columns and stacks of measured leaves, laid out by Taffy into
//! rectangles. Pure: a tree and a way to measure leaves in, a rectangle per node out.

use anyhow::{Result, anyhow, bail};
use taffy::prelude::{
    AlignItems, AlignSelf, AvailableSpace, Dimension, Display, FlexDirection, JustifyContent, LengthPercentage,
    LengthPercentageAuto, NodeId, Position, Rect as Sides, Size, Style as TaffyStyle, TaffyTree, fr, line,
};

/// A length a script gives: pixels, a share of the parent, or the content's own size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Length {
    Px(f32),
    Percent(f32),
    Fit,
}

impl Length {
    /// `24`, `"50%"` or `"fit"`.
    pub fn parse_text(text: &str) -> Result<Self> {
        if text == "fit" {
            return Ok(Self::Fit);
        }
        let percent = text.strip_suffix('%').ok_or_else(|| anyhow!("{text:?} is not a number, \"fit\" or a percentage like \"50%\""))?;
        let value: f32 = percent.trim().parse().map_err(|_| anyhow!("{text:?} is not a percentage like \"50%\""))?;
        Ok(Self::Percent(value / 100.0))
    }

    fn dimension(self) -> Dimension {
        match self {
            Self::Px(px) => Dimension::length(px),
            Self::Percent(share) => Dimension::percent(share),
            Self::Fit => Dimension::auto(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Row,
    Column,
    /// Every child is a layer over the same box, as big as the biggest.
    Stack,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Start,
    Center,
    End,
    Stretch,
}

impl Align {
    pub fn parse(text: &str) -> Result<Self> {
        Ok(match text {
            "start" => Self::Start,
            "center" => Self::Center,
            "end" => Self::End,
            "stretch" => Self::Stretch,
            other => bail!("align {other:?} is not start, center, end or stretch"),
        })
    }

    fn items(self) -> AlignItems {
        match self {
            Self::Start => AlignItems::Start,
            Self::Center => AlignItems::Center,
            Self::End => AlignItems::End,
            Self::Stretch => AlignItems::Stretch,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Justify {
    Start,
    Center,
    End,
    Between,
    Around,
    Evenly,
}

impl Justify {
    pub fn parse(text: &str) -> Result<Self> {
        Ok(match text {
            "start" => Self::Start,
            "center" => Self::Center,
            "end" => Self::End,
            "between" => Self::Between,
            "around" => Self::Around,
            "evenly" => Self::Evenly,
            other => bail!("justify {other:?} is not start, center, end, between, around or evenly"),
        })
    }

    fn content(self) -> JustifyContent {
        match self {
            Self::Start => JustifyContent::Start,
            Self::Center => JustifyContent::Center,
            Self::End => JustifyContent::End,
            Self::Between => JustifyContent::SpaceBetween,
            Self::Around => JustifyContent::SpaceAround,
            Self::Evenly => JustifyContent::SpaceEvenly,
        }
    }
}

/// Where an element out of the flow sits, from its container's edges.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Inset {
    pub left: Option<f32>,
    pub top: Option<f32>,
    pub right: Option<f32>,
    pub bottom: Option<f32>,
}

/// How any node sits in its container.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub w: Length,
    pub h: Length,
    pub grow: f32,
    /// Out of the flow, placed from the container's edges.
    pub absolute: Option<Inset>,
    /// Width over height, so setting one side sizes the other.
    pub aspect: Option<f32>,
    /// How readily it gives up room when its row or column is too small, like text that ellipsizes.
    pub shrink: f32,
}

impl Default for Placement {
    fn default() -> Self {
        Self { w: Length::Fit, h: Length::Fit, grow: 0.0, absolute: None, aspect: None, shrink: 0.0 }
    }
}

/// What a container does with its children.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Arrange {
    pub kind: Kind,
    pub gap: f32,
    /// Top, right, bottom, left.
    pub padding: [f32; 4],
    pub align: Option<Align>,
    pub justify: Option<Justify>,
}

/// `L` is what a leaf draws, `C` what a container draws behind its children.
pub enum Node<L, C> {
    Leaf { placement: Placement, leaf: L },
    Container { placement: Placement, arrange: Arrange, decor: C, children: Vec<Node<L, C>> },
}

/// A laid out rectangle, relative to the root's top-left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Lays `root` out in `space` (the room it has, in pixels) and gives every node's rectangle, in the
/// order a depth-first walk meets them, the root first. `measure` sizes a leaf: known sides (from its
/// size or the container stretching it) and the room it has in, its own width and height out.
pub fn lay_out<L, C>(
    root: &Node<L, C>,
    space: (f32, f32),
    mut measure: impl FnMut(&L, Size<Option<f32>>, Size<AvailableSpace>) -> (f32, f32),
) -> Result<Vec<Rect>> {
    let mut tree: TaffyTree<&L> = TaffyTree::new();
    let top = build(&mut tree, root, None)?;
    let available = Size { width: AvailableSpace::Definite(space.0), height: AvailableSpace::Definite(space.1) };
    tree.compute_layout_with_measure(top, available, |known, available, _, leaf, _| match leaf {
        Some(leaf) => {
            let (width, height) = measure(leaf, known, available);
            Size { width: known.width.unwrap_or(width), height: known.height.unwrap_or(height) }
        }
        None => Size::ZERO,
    })
    .map_err(|e| anyhow!("laying the widget out: {e}"))?;
    let mut rects = Vec::new();
    collect(&tree, top, root, (0.0, 0.0), &mut rects)?;
    Ok(rects)
}

fn build<'a, L, C>(tree: &mut TaffyTree<&'a L>, node: &'a Node<L, C>, parent: Option<Kind>) -> Result<NodeId> {
    let taffy = |e| anyhow!("laying the widget out: {e}");
    match node {
        Node::Leaf { placement, leaf } => tree.new_leaf_with_context(style(placement, parent, None), leaf).map_err(taffy),
        Node::Container { placement, arrange, children, .. } => {
            let children = children.iter().map(|child| build(tree, child, Some(arrange.kind))).collect::<Result<Vec<_>>>()?;
            tree.new_with_children(style(placement, parent, Some(arrange)), &children).map_err(taffy)
        }
    }
}

fn style(placement: &Placement, parent: Option<Kind>, arrange: Option<&Arrange>) -> TaffyStyle {
    let mut style = TaffyStyle {
        size: Size { width: placement.w.dimension(), height: placement.h.dimension() },
        flex_grow: placement.grow,
        flex_shrink: if placement.grow > 0.0 { placement.shrink.max(1.0) } else { placement.shrink },
        aspect_ratio: placement.aspect,
        ..TaffyStyle::default()
    };
    if let Some(inset) = placement.absolute {
        let side = |value: Option<f32>| value.map_or(LengthPercentageAuto::auto(), LengthPercentageAuto::length);
        style.position = Position::Absolute;
        style.inset = Sides { left: side(inset.left), right: side(inset.right), top: side(inset.top), bottom: side(inset.bottom) };
    } else if parent == Some(Kind::Stack) {
        style.grid_row = line(1);
        style.grid_column = line(1);
    }
    if let Some(arrange) = arrange {
        let [top, right, bottom, left] = arrange.padding.map(LengthPercentage::length);
        style.padding = Sides { top, right, bottom, left };
        style.align_items = arrange.align.map(Align::items);
        style.justify_items = arrange.align.map(Align::items);
        style.justify_content = arrange.justify.map(Justify::content);
        match arrange.kind {
            Kind::Row | Kind::Column => {
                style.display = Display::Flex;
                style.flex_direction = if arrange.kind == Kind::Row { FlexDirection::Row } else { FlexDirection::Column };
                style.gap = Size { width: LengthPercentage::length(arrange.gap), height: LengthPercentage::length(arrange.gap) };
            }
            Kind::Stack => {
                style.display = Display::Grid;
                style.grid_template_rows = vec![fr(1.0_f32)];
                style.grid_template_columns = vec![fr(1.0_f32)];
            }
        }
    }
    if parent == Some(Kind::Stack) && placement.absolute.is_none() && placement.grow > 0.0 {
        style.align_self = Some(AlignSelf::Stretch);
        style.justify_self = Some(AlignSelf::Stretch);
    }
    style
}

fn collect<L, C>(tree: &TaffyTree<&L>, id: NodeId, node: &Node<L, C>, origin: (f32, f32), rects: &mut Vec<Rect>) -> Result<()> {
    let laid = tree.layout(id).map_err(|e| anyhow!("reading the layout: {e}"))?;
    let at = (origin.0 + laid.location.x, origin.1 + laid.location.y);
    rects.push(Rect { x: at.0, y: at.1, w: laid.size.width, h: laid.size.height });
    if let Node::Container { children, .. } = node {
        let ids = tree.children(id).map_err(|e| anyhow!("reading the layout: {e}"))?;
        for (child_id, child) in ids.into_iter().zip(children) {
            collect(tree, child_id, child, at, rects)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A leaf of a fixed natural size, like a word of text.
    type TestNode = Node<(f32, f32), ()>;

    fn fixed(w: f32, h: f32) -> TestNode {
        Node::Leaf { placement: Placement::default(), leaf: (w, h) }
    }

    fn container(kind: Kind, gap: f32, children: Vec<TestNode>) -> TestNode {
        let arrange = Arrange { kind, gap, padding: [0.0; 4], align: Some(Align::Start), justify: None };
        Node::Container { placement: Placement { w: Length::Percent(1.0), h: Length::Percent(1.0), ..Placement::default() }, arrange, decor: (), children }
    }

    fn lay(root: &TestNode) -> Vec<Rect> {
        lay_out(root, (200.0, 100.0), |&(w, h), _, _| (w, h)).unwrap()
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn a_row_places_children_left_to_right_with_its_gap() {
        let rects = lay(&container(Kind::Row, 10.0, vec![fixed(30.0, 20.0), fixed(40.0, 10.0)]));
        assert_eq!(rects, [rect(0.0, 0.0, 200.0, 100.0), rect(0.0, 0.0, 30.0, 20.0), rect(40.0, 0.0, 40.0, 10.0)]);
    }

    #[test]
    fn a_column_stacks_children_downwards_with_its_gap() {
        let rects = lay(&container(Kind::Column, 5.0, vec![fixed(30.0, 20.0), fixed(40.0, 10.0)]));
        assert_eq!(rects[1..], [rect(0.0, 0.0, 30.0, 20.0), rect(0.0, 25.0, 40.0, 10.0)]);
    }

    #[test]
    fn growing_children_share_the_room_that_is_left() {
        let grow = |n: f32| Node::Leaf { placement: Placement { grow: n, ..Placement::default() }, leaf: (0.0, 10.0) };
        let rects = lay(&container(Kind::Row, 0.0, vec![fixed(50.0, 10.0), grow(1.0), grow(2.0)]));
        assert_eq!(rects[2].w, 50.0);
        assert_eq!(rects[3].w, 100.0);
        assert_eq!(rects[3].x, 100.0);
    }

    #[test]
    fn percentages_are_of_the_parent() {
        let half = Node::Leaf { placement: Placement { w: Length::Percent(0.5), ..Placement::default() }, leaf: (0.0, 10.0) };
        assert_eq!(lay(&container(Kind::Row, 0.0, vec![half]))[1].w, 100.0);
    }

    #[test]
    fn padding_moves_children_in_from_the_edges() {
        let mut root = container(Kind::Row, 0.0, vec![fixed(10.0, 10.0)]);
        if let Node::Container { arrange, .. } = &mut root {
            arrange.padding = [4.0, 0.0, 0.0, 6.0];
        }
        assert_eq!(lay(&root)[1], rect(6.0, 4.0, 10.0, 10.0));
    }

    #[test]
    fn a_stack_layers_its_children_in_one_box_as_big_as_the_biggest() {
        let stack = Node::Container {
            placement: Placement::default(),
            arrange: Arrange { kind: Kind::Stack, gap: 0.0, padding: [0.0; 4], align: Some(Align::Center), justify: None },
            decor: (),
            children: vec![fixed(60.0, 40.0), fixed(20.0, 10.0)],
        };
        let rects = lay(&container(Kind::Row, 0.0, vec![stack]));
        assert_eq!(rects[1], rect(0.0, 0.0, 60.0, 40.0));
        assert_eq!(rects[2], rect(0.0, 0.0, 60.0, 40.0));
        assert_eq!(rects[3], rect(20.0, 15.0, 20.0, 10.0), "centred over the first layer");
    }

    #[test]
    fn an_absolute_child_sits_from_its_containers_edges_out_of_the_flow() {
        let badge = Node::Leaf {
            placement: Placement { absolute: Some(Inset { right: Some(4.0), top: Some(2.0), ..Inset::default() }), ..Placement::default() },
            leaf: (12.0, 12.0),
        };
        let rects = lay(&container(Kind::Row, 10.0, vec![badge, fixed(30.0, 20.0)]));
        assert_eq!(rects[1], rect(184.0, 2.0, 12.0, 12.0));
        assert_eq!(rects[2], rect(0.0, 0.0, 30.0, 20.0), "the badge takes no room in the row");
    }

    #[test]
    fn nested_containers_place_their_children_inside_themselves() {
        let inner = Node::Container {
            placement: Placement::default(),
            arrange: Arrange { kind: Kind::Column, gap: 2.0, padding: [0.0; 4], align: Some(Align::Start), justify: None },
            decor: (),
            children: vec![fixed(10.0, 10.0), fixed(10.0, 10.0)],
        };
        let rects = lay(&container(Kind::Row, 8.0, vec![fixed(20.0, 20.0), inner]));
        assert_eq!(rects[2], rect(28.0, 0.0, 10.0, 22.0));
        assert_eq!(rects[4], rect(28.0, 12.0, 10.0, 10.0));
    }

    #[test]
    fn leaves_are_measured_against_the_room_they_have() {
        let root = container(Kind::Column, 0.0, vec![fixed(0.0, 0.0)]);
        let mut widths = Vec::new();
        lay_out(&root, (120.0, 50.0), |_, _, available| {
            if let AvailableSpace::Definite(w) = available.width {
                widths.push(w);
            }
            (10.0, 10.0)
        })
        .unwrap();
        assert!(widths.contains(&120.0));
    }

    #[test]
    fn lengths_read_percentages_and_fit() {
        assert_eq!(Length::parse_text("50%").unwrap(), Length::Percent(0.5));
        assert_eq!(Length::parse_text("fit").unwrap(), Length::Fit);
        assert!(Length::parse_text("wide").is_err());
    }
}
