use crate::ui::App;
use eframe::egui::{self, ViewportCommand, ViewportEvent, ViewportId};
use egui_software_backend::{BufferMutRef, ColorFieldOrder, EguiSoftwareRender};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{
    num::NonZeroU32,
    rc::Rc,
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId},
};

const BACKGROUND_FRAME_INTERVAL: Duration = Duration::from_millis(250);

pub fn run(viewport: egui::ViewportBuilder) -> anyhow::Result<()> {
    let event_loop = EventLoop::<Instant>::with_user_event().build()?;
    let ctx = egui::Context::default();
    let proxy = event_loop.create_proxy();
    ctx.set_request_repaint_callback(move |request| {
        if let Some(deadline) = Instant::now().checked_add(request.delay) {
            let _ = proxy.send_event(deadline);
        }
    });
    let mut runtime = Runtime {
        ctx,
        viewport,
        window: None,
        repaint_at: None,
        result: Ok(()),
    };
    event_loop.run_app(&mut runtime)?;
    runtime.result
}

struct Runtime {
    ctx: egui::Context,
    viewport: egui::ViewportBuilder,
    window: Option<PlayerWindow>,
    repaint_at: Option<Instant>,
    result: anyhow::Result<()>,
}

impl ApplicationHandler<Instant> for Runtime {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            match PlayerWindow::new(event_loop, &self.ctx, &self.viewport) {
                Ok(window) => {
                    window.window.request_redraw();
                    self.window = Some(window);
                }
                Err(error) => {
                    self.result = Err(error);
                    event_loop.exit();
                }
            }
        }
    }

    fn user_event(&mut self, _: &ActiveEventLoop, deadline: Instant) {
        self.repaint_at = Some(self.repaint_at.map_or(deadline, |old| old.min(deadline)));
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(mut deadline) = self.repaint_at {
            if let Some(window) = &self.window
                && !window.window.has_focus()
                && let Some(last_frame) = window.last_frame
            {
                deadline = deadline.max(last_frame + BACKGROUND_FRAME_INTERVAL);
            }
            if deadline <= Instant::now() {
                self.repaint_at = None;
                if let Some(window) = &self.window {
                    window.window.request_redraw();
                }
            } else {
                event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
                return;
            }
        }
        event_loop.set_control_flow(ControlFlow::Wait);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(window) = self
            .window
            .as_mut()
            .filter(|window| window.window.id() == id)
        else {
            return;
        };
        match event {
            WindowEvent::RedrawRequested => {
                if let Err(error) = window.redraw(&self.ctx, event_loop) {
                    self.result = Err(error);
                    event_loop.exit();
                }
            }
            WindowEvent::CloseRequested => {
                window.close_requested = true;
                window.window.request_redraw();
            }
            WindowEvent::Destroyed => event_loop.exit(),
            _ => {
                let response = window.input.on_window_event(&window.window, &event);
                if response.repaint {
                    window.window.request_redraw();
                }
            }
        }
    }
}

struct PlayerWindow {
    window: Rc<Window>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
    renderer: EguiSoftwareRender,
    input: egui_winit::State,
    app: App,
    close_requested: bool,
    last_frame: Option<Instant>,
    textures: egui::TexturesDelta,
}

impl PlayerWindow {
    fn new(
        event_loop: &ActiveEventLoop,
        ctx: &egui::Context,
        viewport: &egui::ViewportBuilder,
    ) -> anyhow::Result<Self> {
        let window = Rc::new(egui_winit::create_window(ctx, event_loop, viewport)?);
        let display =
            softbuffer::Context::new(window.clone()).map_err(|error| anyhow::anyhow!("{error}"))?;
        let surface = softbuffer::Surface::new(&display, window.clone())
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let input = egui_winit::State::new(
            ctx.clone(),
            ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        let hwnd = match window.window_handle()?.as_raw() {
            RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
            _ => None,
        };
        Ok(Self {
            window,
            surface,
            renderer: EguiSoftwareRender::new(ColorFieldOrder::Bgra).with_caching(false),
            input,
            app: App::new_internal(ctx, true, hwnd),
            close_requested: false,
            last_frame: None,
            textures: egui::TexturesDelta::default(),
        })
    }

    fn redraw(&mut self, ctx: &egui::Context, event_loop: &ActiveEventLoop) -> anyhow::Result<()> {
        if !self.close_requested
            && (self.window.is_visible() == Some(false) || self.window.is_minimized() == Some(true))
        {
            return Ok(());
        }
        let mut input = self.input.take_egui_input(&self.window);
        let info = input.viewports.entry(ViewportId::ROOT).or_default();
        egui_winit::update_viewport_info(info, ctx, &self.window, false);
        let mut close = std::mem::take(&mut self.close_requested);
        if close {
            info.events.push(ViewportEvent::Close);
        }
        let mut output = ctx.run(input, |ctx| self.app.update(ctx));
        self.input
            .handle_platform_output(&self.window, output.platform_output);
        if let Some(viewport) = output.viewport_output.get_mut(&ViewportId::ROOT) {
            viewport.commands.retain(|command| match command {
                ViewportCommand::Close | ViewportCommand::CancelClose => {
                    close = matches!(command, ViewportCommand::Close);
                    false
                }
                _ => true,
            });
            egui_winit::process_viewport_commands(
                ctx,
                &mut egui::ViewportInfo::default(),
                std::mem::take(&mut viewport.commands),
                &self.window,
                &mut Vec::new(),
            );
        }
        if close {
            event_loop.exit();
            return Ok(());
        }
        self.textures.append(output.textures_delta);
        if self.window.is_visible() == Some(false) {
            let one = NonZeroU32::MIN;
            return self
                .surface
                .resize(one, one)
                .map_err(|error| anyhow::anyhow!("{error}"));
        }
        let size = self.window.inner_size();
        if let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        {
            self.surface
                .resize(width, height)
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            let mut buffer = self
                .surface
                .buffer_mut()
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            buffer.fill(0);
            let primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
            self.renderer.render(
                &mut BufferMutRef::new(
                    bytemuck::cast_slice_mut(&mut buffer),
                    size.width as usize,
                    size.height as usize,
                ),
                &primitives,
                &std::mem::take(&mut self.textures),
                output.pixels_per_point,
            );
            self.window.pre_present_notify();
            buffer
                .present()
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            self.last_frame = Some(Instant::now());
        }
        Ok(())
    }
}
