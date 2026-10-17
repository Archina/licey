// Draws a pre-rendered texture over the current viewport (a fullscreen triangle in clip
// space). Used to present the offscreen canvas that the mslicer passes render into.
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;

@vertex
fn vert(@builtin(vertex_index) index: u32) -> @builtin(position) vec4f {
    // Fullscreen triangle. The egui pass sets the viewport to the paint callback's rect before
    // calling us, so this only covers that rect.
    var positions = array<vec2f, 3>(
        vec2f(-1.0, -1.0),
        vec2f(3.0, -1.0),
        vec2f(-1.0, 3.0),
    );

    return vec4f(positions[index], 0.0, 1.0);
}

// 0-1 linear from 0-1 sRGB gamma. Mirrors `common::color::SRgb::to_linear_rgb`.
fn linear_from_gamma_rgb(rgb: vec3f) -> vec3f {
    let cutoff = rgb < vec3f(0.04045);
    let lower = rgb / vec3f(12.92);
    let higher = pow((rgb + vec3f(0.055)) / vec3f(1.055), vec3f(2.4));
    return select(higher, lower, cutoff);
}

@fragment
fn frag(@builtin(position) position: vec4f) -> @location(0) vec4f {
    // The canvas covers the whole window and shares the egui pass' target coordinate space, so
    // the callback's rect is at the same absolute pixels in both. Sampling at the fragment's
    // framebuffer position is therefore an exact 1:1 copy of the rect the mslicer passes drew
    // into.
    let uv = position.xy / vec2f(textureDimensions(source));
    let source_gamma = textureSample(source, source_sampler, uv);

    // The canvas holds display-referred (gamma encoded) values, because that is the space the
    // mslicer passes were authored for. The egui pass renders into an sRGB converting target, so
    // hand it linear values and let the target's own encoding put the original gamma values back.
    // Alpha is never gamma encoded (and the alpha blending is premultiplied).
    return vec4f(linear_from_gamma_rgb(source_gamma.rgb), source_gamma.a);
}
