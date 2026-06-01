use smithay::{
    backend::renderer::{
        damage::{Error as OutputDamageTrackerError, OutputDamageTracker, RenderOutputResult},
        element::{
            solid::SolidColorRenderElement,
            surface::WaylandSurfaceRenderElement,
            utils::{
                ConstrainAlign, ConstrainScaleBehavior, CropRenderElement, RelocateRenderElement,
                RescaleRenderElement,
            },
            AsRenderElements, RenderElement, Wrap,
        },
        Color32F, ImportAll, ImportMem, Renderer,
    },
    desktop::space::{
        constrain_space_element, ConstrainBehavior, ConstrainReference, Space, SpaceRenderElements,
    },
    output::Output,
    utils::{Point, Rectangle, Size},
};

#[cfg(feature = "debug")]
use crate::drawing::FpsElement;
use crate::{
    drawing::{PointerRenderElement, CLEAR_COLOR, CLEAR_COLOR_FULLSCREEN},
    shell::{FullscreenSurface, WindowElement, WindowRenderElement},
};

smithay::backend::renderer::element::render_elements! {
    pub CustomRenderElements<R> where
        R: ImportAll + ImportMem;
    Pointer=PointerRenderElement<R>,
    Surface=WaylandSurfaceRenderElement<R>,
    SnapPreview=SolidColorRenderElement,
    #[cfg(feature = "debug")]
    // Note: We would like to borrow this element instead, but that would introduce
    // a feature-dependent lifetime, which introduces a lot more feature bounds
    // as the whole type changes and we can't have an unused lifetime (for when "debug" is disabled)
    // in the declaration.
    Fps=FpsElement<R::TextureId>,
}

impl<R: Renderer> std::fmt::Debug for CustomRenderElements<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pointer(arg0) => f.debug_tuple("Pointer").field(arg0).finish(),
            Self::Surface(arg0) => f.debug_tuple("Surface").field(arg0).finish(),
            Self::SnapPreview(arg0) => f.debug_tuple("SnapPreview").field(arg0).finish(),
            #[cfg(feature = "debug")]
            Self::Fps(arg0) => f.debug_tuple("Fps").field(arg0).finish(),
            Self::_GenericCatcher(arg0) => f.debug_tuple("_GenericCatcher").field(arg0).finish(),
        }
    }
}

smithay::backend::renderer::element::render_elements! {
    pub OutputRenderElements<R, E> where R: ImportAll + ImportMem;
    Space=SpaceRenderElements<R, E>,
    Window=Wrap<E>,
    Custom=CustomRenderElements<R>,
    Preview=CropRenderElement<RelocateRenderElement<RescaleRenderElement<WindowRenderElement<R>>>>,
    Mirror=smithay::backend::renderer::element::texture::TextureRenderElement<<R as smithay::backend::renderer::RendererSuper>::TextureId>,
    HeadClone=RelocateRenderElement<RescaleRenderElement<SpaceRenderElements<R, E>>>,
    AiCursor=smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement<R>,
    Selection=SolidColorRenderElement,
}

impl<R: Renderer + ImportAll + ImportMem, E: RenderElement<R> + std::fmt::Debug> std::fmt::Debug
    for OutputRenderElements<R, E>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Space(arg0) => f.debug_tuple("Space").field(arg0).finish(),
            Self::Window(arg0) => f.debug_tuple("Window").field(arg0).finish(),
            Self::Custom(arg0) => f.debug_tuple("Custom").field(arg0).finish(),
            Self::Preview(arg0) => f.debug_tuple("Preview").field(arg0).finish(),
            Self::Mirror(arg0) => f.debug_tuple("Mirror").field(arg0).finish(),
            Self::HeadClone(arg0) => f.debug_tuple("HeadClone").field(arg0).finish(),
            Self::AiCursor(arg0) => f.debug_tuple("AiCursor").field(arg0).finish(),
            Self::Selection(arg0) => f.debug_tuple("Selection").field(arg0).finish(),
            Self::_GenericCatcher(arg0) => f.debug_tuple("_GenericCatcher").field(arg0).finish(),
        }
    }
}

pub fn space_preview_elements<'a, R, C>(
    renderer: &'a mut R,
    space: &'a Space<WindowElement>,
    output: &'a Output,
) -> impl Iterator<Item = C> + 'a
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
    C: From<CropRenderElement<RelocateRenderElement<RescaleRenderElement<WindowRenderElement<R>>>>> + 'a,
{
    let constrain_behavior = ConstrainBehavior {
        reference: ConstrainReference::BoundingBox,
        behavior: ConstrainScaleBehavior::Fit,
        align: ConstrainAlign::CENTER,
    };

    let preview_padding = 10;

    let elements_on_space = space.elements_for_output(output).count();
    let output_scale = output.current_scale().fractional_scale();
    let output_transform = output.current_transform();
    let output_size = output
        .current_mode()
        .map(|mode| {
            output_transform
                .transform_size(mode.size)
                .to_f64()
                .to_logical(output_scale)
        })
        .unwrap_or_default();

    let max_elements_per_row = 4;
    let elements_per_row = usize::min(elements_on_space, max_elements_per_row);
    let rows = f64::ceil(elements_on_space as f64 / elements_per_row as f64);

    let preview_size = Size::from((
        f64::round(output_size.w / elements_per_row as f64) as i32 - preview_padding * 2,
        f64::round(output_size.h / rows) as i32 - preview_padding * 2,
    ));

    space
        .elements_for_output(output)
        .enumerate()
        .flat_map(move |(element_index, window)| {
            let column = element_index % elements_per_row;
            let row = element_index / elements_per_row;
            let preview_location = Point::from((
                preview_padding + (preview_padding + preview_size.w) * column as i32,
                preview_padding + (preview_padding + preview_size.h) * row as i32,
            ));
            let constrain = Rectangle::new(preview_location, preview_size);
            constrain_space_element(
                renderer,
                window,
                preview_location,
                1.0,
                output_scale,
                constrain,
                constrain_behavior,
            )
        })
}

#[profiling::function]
pub fn output_elements<R>(
    output: &Output,
    space: &Space<WindowElement>,
    custom_elements: impl IntoIterator<Item = CustomRenderElements<R>>,
    renderer: &mut R,
    show_window_preview: bool,
) -> (Vec<OutputRenderElements<R, WindowRenderElement<R>>>, Color32F)
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    if let Some(window) = output
        .user_data()
        .get::<FullscreenSurface>()
        .and_then(|f| f.get())
    {
        let scale = output.current_scale().fractional_scale().into();
        let zone = compstr::placement::topbar_safe_zone(output);
        let fullscreen_origin = (zone.loc.x, zone.loc.y).into();
        let window_render_elements: Vec<WindowRenderElement<R>> =
            AsRenderElements::<R>::render_elements(&window, renderer, fullscreen_origin, scale, 1.0);

        let elements = custom_elements
            .into_iter()
            .map(OutputRenderElements::from)
            .chain(
                window_render_elements
                    .into_iter()
                    .map(|e| OutputRenderElements::Window(Wrap::from(e))),
            )
            .collect::<Vec<_>>();
        (elements, CLEAR_COLOR_FULLSCREEN)
    } else {
        let mut output_render_elements = custom_elements
            .into_iter()
            .map(OutputRenderElements::from)
            .collect::<Vec<_>>();

        if show_window_preview && space.elements_for_output(output).count() > 0 {
            output_render_elements.extend(space_preview_elements(renderer, space, output));
        }

        let space_elements = smithay::desktop::space::space_render_elements::<_, WindowElement, _>(
            renderer,
            [space],
            output,
            1.0,
        )
        .expect("output without mode?");
        output_render_elements.extend(space_elements.into_iter().map(OutputRenderElements::Space));

        (output_render_elements, CLEAR_COLOR)
    }
}

/// COMPSTR-HDMI-CLONE-001: build a render-element list that CLONES `src_output`'s
/// LIVE surface set (windows + layer-shell — i.e. cpit's taskbar/viewport + ws0's
/// daedal/kiosk) onto `dst_output`, aspect-preserved and centered (letterbox).
///
/// This is the NON-blit path: it re-renders the SAME live wl_surfaces with the SAME
/// renderer that drew them on the source head — it never moves a finished buffer
/// between outputs, so it cannot hit the import_dmabuf `Error::DeviceMissing` that
/// killed the blit (BLIT-002). The fit (uniform scale + centering offset) comes from
/// `compstr::clone::letterbox`; the wrappers forward the inner element id/commit, so
/// `dst_output`'s own damage tracker repaints when the source surfaces commit. The
/// caller clears `dst_output` to opaque black so the letterbox bars are solid.
pub fn clone_space_elements<R>(
    renderer: &mut R,
    src_space: &Space<WindowElement>,
    src_output: &Output,
    dst_output: &Output,
) -> Vec<OutputRenderElements<R, WindowRenderElement<R>>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    use smithay::backend::renderer::element::utils::Relocate;
    use smithay::utils::Scale;

    let src_size = src_output.current_mode().map(|m| m.size).unwrap_or_default();
    let dst_size = dst_output.current_mode().map(|m| m.size).unwrap_or_default();
    if src_size.w == 0 || src_size.h == 0 || dst_size.w == 0 || dst_size.h == 0 {
        return Vec::new();
    }

    let fit = compstr::clone::letterbox(src_size, dst_size);

    let space_elements = smithay::desktop::space::space_render_elements::<_, WindowElement, _>(
        renderer,
        [src_space],
        src_output,
        1.0,
    )
    .expect("clone: source output without mode?");

    space_elements
        .into_iter()
        .map(|e| {
            // Scale each source element about the source top-left (0,0), then offset
            // it into the centered letterbox rectangle on the destination head.
            let scaled =
                RescaleRenderElement::from_element(e, Point::from((0, 0)), Scale::from(fit.scale));
            let placed =
                RelocateRenderElement::from_element(scaled, fit.offset, Relocate::Relative);
            OutputRenderElements::HeadClone(placed)
        })
        .collect()
}

/// COMPSTR-004-AI-WORKSPACE-DIRECT-PRESENT: present the AI workspace's LIVE
/// surfaces (the Xwayland-rootful chromium kiosk) DIRECTLY on `dst_output`
/// (eDP-1) on the peacock toggle, with the drishti AI cursor on top.
///
/// This is the B-live path: it re-renders the AI space's wl_surfaces with the
/// SAME renderer (no export DMA-BUF, no cpit quad). It reuses `clone_space_elements`
/// for the surfaces, so when `ai_output`'s mode == `dst_output`'s mode the fit
/// collapses to identity (fullscreen 1:1) and the human pointer lands on the
/// surface at true coords. The drishti rides on top at the AI pointer position
/// (identity/1:1 placement — AI-space logical coords == eDP coords at 1:1).
pub fn present_ai_elements<R>(
    renderer: &mut R,
    ai_space: &Space<WindowElement>,
    ai_output: &Output,
    dst_output: &Output,
    ai_pointer_pos: Point<f64, smithay::utils::Logical>,
) -> Vec<OutputRenderElements<R, WindowRenderElement<R>>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    use compstr::screen::ai_cursor::{drishti_buffer, DRISHTI_HOTSPOT};
    use smithay::backend::renderer::element::{memory::MemoryRenderBufferRenderElement, Kind};

    let mut elements: Vec<OutputRenderElements<R, WindowRenderElement<R>>> = Vec::new();

    // drishti AI cursor on top (1:1 placement; identity fit only).
    let px = ai_pointer_pos.x.round() as i32;
    let py = ai_pointer_pos.y.round() as i32;
    let drishti_loc = Point::<f64, smithay::utils::Physical>::from((
        (px - DRISHTI_HOTSPOT) as f64,
        (py - DRISHTI_HOTSPOT) as f64,
    ));
    match MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        drishti_loc,
        drishti_buffer(),
        None,
        None,
        None,
        Kind::Cursor,
    ) {
        Ok(sprite) => elements.push(OutputRenderElements::AiCursor(sprite)),
        Err(e) => tracing::warn!("present: drishti buffer upload failed: {e:?}"),
    }

    // AI surfaces underneath (identity fit when ai_output mode == dst_output mode).
    elements.extend(clone_space_elements(renderer, ai_space, ai_output, dst_output));
    elements
}

#[allow(clippy::too_many_arguments)]
pub fn render_output<'a, 'd, R>(
    output: &'a Output,
    space: &'a Space<WindowElement>,
    custom_elements: impl IntoIterator<Item = CustomRenderElements<R>>,
    renderer: &'a mut R,
    framebuffer: &'a mut R::Framebuffer<'_>,
    damage_tracker: &'d mut OutputDamageTracker,
    age: usize,
    show_window_preview: bool,
) -> Result<RenderOutputResult<'d>, OutputDamageTrackerError<R::Error>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    let (elements, clear_color) =
        output_elements(output, space, custom_elements, renderer, show_window_preview);
    damage_tracker.render_output(renderer, framebuffer, age, &elements, clear_color)
}
