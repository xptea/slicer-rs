# Slicer patch

Slicer uses a non-blocking `wgpu::PollType::Poll` during drawable resize.
The upstream renderer waits for every in-flight submission before replacing
intermediate textures. During interactive window resizing that stalls GPUI's
event loop and exposes the transparent swapchain clear, producing visible
black or color flashes. WGPU defers resource destruction until the GPU is
finished with a texture, so polling without waiting keeps resize responsive.
