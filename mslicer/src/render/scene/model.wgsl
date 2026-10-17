// The scene viewport's model/supports shader: the pre-bevy mslicer model pass
// (`mslicer/shaders/workspace/model/base.wgsl` at 041fb5a) ported onto a bevy material.
//
// The old pass rendered into a non-sRGB (`Rgba8Unorm`) canvas, so it worked in display-referred
// (sRGB-encoded) space and its hand-picked colours (`OVERHANG_COLOR`, `SELECTED_COLOR`, the
// back-face grey) are display values. Bevy renders into an `Rgba8UnormSrgb` image target, which
// sRGB-encodes whatever the fragment stage outputs, so everything below still computes display
// values and the single `display_to_linear` at the end undoes the target's encoding.
//
// The old pass also wrote the picking buffer (`@location(1) model` + the normal and world
// targets); bevy's scene picking uses `MeshRayCast` instead, so only the colour survives.
// The old deferred lighting pass is folded in here as a single directional light (see `shade`).

enable primitive_index;

// `bevy_pbr::material::MATERIAL_BIND_GROUP_INDEX`, which the mesh pipeline always binds 3rd.
@group(3) @binding(0) var<uniform> params: ModelParams;
@group(3) @binding(1) var<storage, read> selected: array<u32>;

const STYLE_NORMAL: u32 = 0u;
const STYLE_RANDOM: u32 = 1u;
const STYLE_RENDERED: u32 = 2u;

const SELECTED_COLOR: vec3<f32> = vec3<f32>(0.2, 0.2, 1.0);
const OOB_COLOR: vec3<f32> = vec3<f32>(1.0, 0.0, 0.0);
const OVERHANG_COLOR: vec3<f32> = vec3<f32>(0.67, 0.65, 0.38);

struct ModelParams {
    model_color: vec3<f32>,
    build_volume: vec3<f32>,
    render_style: u32,
    overhang_angle: f32,
}

// The parts of the mesh pipeline's `VertexOutput` this pass reads. The scene's meshes carry
// positions and indices only, so `VERTEX_NORMALS` is off and the face normal has to come from
// the world position's screen-space derivatives, exactly as the old shader computed it.
struct VertexOutput {
    @builtin(position) frag_coord: vec4<f32>,
    @location(0) world_position: vec4<f32>,
}

var<private> seed: u32 = 0u;

fn rand() -> f32 {
    seed = seed * 747796405u + 2891336453u;
    let f = f32(seed >> 9u) / f32(1u << 23u);
    return fract(f);
}

fn face_normal(world_position: vec3<f32>) -> vec3<f32> {
    let dy = dpdy(world_position);
    let dx = dpdx(world_position);
    return normalize(cross(dy, dx));
}

// `primitive_index` is the triangle index within the draw, and `mesh_from_slicer` flattens
// `Mesh::faces()` in order, so it is the face index this bitmask is keyed by.
fn is_selected(face: u32) -> bool {
    let word = face / 32u;
    let bit = face % 32u;
    return ((selected[word] >> bit) & 1u) != 0u;
}

// The old renderer shaded models in a separate deferred pass (a camera-relative Blinn-Phong
// light over a normal G-buffer); that pass did not survive the port, which left the models flat.
// Lighting is folded into the material here instead: one fixed key light in mslicer's Z-up world
// space plus a constant ambient term. Faces are lit from above/front-left, so orbiting the camera
// reveals shape without the light following it.
fn shade(normal: vec3<f32>, is_front: bool) -> f32 {
    // Nothing is culled: back faces are shaded too, so flip their normal to face the light.
    let n = select(-normal, normal, is_front);
    let light = normalize(vec3<f32>(0.45, -0.55, 0.70));
    return 0.3 + 0.7 * max(dot(n, light), 0.0);
}

fn outside_build_volume(pos: vec3<f32>) -> bool {
    let build = params.build_volume / 2.0;
    return pos.x < -build.x || pos.x > build.x
        || pos.y < -build.y || pos.y > build.y
        || pos.z < -0.01    || pos.z > params.build_volume.z;
}

fn render(is_front: bool, world_position: vec3<f32>, index: u32, normal: vec3<f32>) -> vec3<f32> {
    switch params.render_style {
        case STYLE_NORMAL: {
            return normal * 0.5 + 0.5;
        }
        case STYLE_RANDOM: {
            seed = index;
            return vec3<f32>(rand(), rand(), rand());
        }
        case STYLE_RENDERED: {
            var color = params.model_color;
            if bitcast<u32>(params.overhang_angle) != 0xFFFFFFFFu {
                color = mix(color, OVERHANG_COLOR, 1.0 - smoothstep(0.0, params.overhang_angle, acos(-normal.z)));
            }

            color = select(color, SELECTED_COLOR, is_selected(index));
            color = select(vec3<f32>(0.5), color, is_front);
            color = select(color, OOB_COLOR, outside_build_volume(world_position));

            return color * shade(normal, is_front);
        }
        // Unreachable: `RenderStyle` has exactly the three cases above.
        default: {
            return vec3<f32>();
        }
    }
}

// The inverse of the sRGB transfer function the `Rgba8UnormSrgb` view target applies on write,
// i.e. what egui ends up seeing is the value `render` returned.
fn display_to_linear(c: vec3<f32>) -> vec3<f32> {
    return select(c / 12.92, pow((c + 0.055) / 1.055, vec3<f32>(2.4)), c > vec3<f32>(0.04045));
}

@fragment
fn fragment(
    in: VertexOutput,
    @builtin(front_facing) is_front: bool,
    @builtin(primitive_index) index: u32,
) -> @location(0) vec4<f32> {
    let normal = face_normal(in.world_position.xyz);
    let color = render(is_front, in.world_position.xyz, index, normal);

    return vec4<f32>(display_to_linear(color), 1.0);
}
