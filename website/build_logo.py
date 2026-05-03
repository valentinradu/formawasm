"""Build the formawasm logo in Blender and render to logo.png.

Run headless from project root:
    blender --background --python website/build_logo.py

Output: website/logo.png (512x512 PNG with transparent background)
"""

import math
import os

import bmesh
import bpy
from mathutils import Vector

# ---------- parameters ----------
FOOTPRINT = 1.0          # half-width of box X/Y
LAYER_H = 0.30           # bottom layer (magenta) — uniform thickness
CORNER_RADIUS = 0.32     # vertical-corner roundness (the "soft toy" base corners)
CORNER_SEG = 16          # arc segments per corner

# Top cyan layer becomes a wedge: thin at -Y (below the text baseline) so the
# ramp slopes downward toward the bottom of the text.
TOP_H_THIN = 0.10
TOP_H_THICK = 0.40
TOP_H_AVG = (TOP_H_THIN + TOP_H_THICK) / 2
TOP_SLOPE = (TOP_H_THICK - TOP_H_THIN) / (2 * FOOTPRINT)
TOP_TILT = math.atan(TOP_SLOPE)

CAVITY_DEPTH = 0.034     # slightly less than 2× shallow

# Bevel applied to the top after the boolean cuts — softens the cavity rim
# (text edges) and the top face's outer edge.
TOP_BEVEL_WIDTH = 0.012
TOP_BEVEL_SEGMENTS = 3

# Debossed text — three-letter "fwa" needs a tighter size + further-left dot
# than the formalang two-letter "fv" so the lettermark stays balanced.
TEXT_SIZE = 1.05
TEXT_FONT = "fonts/Hack-Bold.ttf"  # MIT-licensed mono font
FWA_X = 0.10
DOT_X = -0.85

# Toon palette: (shadow, mid, highlight) — Tokyo Night magenta + cyan, with
# darker shadow and brighter highlight to give the cel shader range.
MAGENTA_SHADOW = "#3b2470"
MAGENTA_MID = "#9d7cd8"
MAGENTA_LIGHT = "#c4a8ff"

CYAN_SHADOW = "#1f6f8c"
CYAN_MID = "#7dcfff"
CYAN_LIGHT = "#b4e8ff"

OUT_W, OUT_H = 512, 512
SAMPLES = 32

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_PATH = os.path.join(SCRIPT_DIR, "logo.png")


# ---------- helpers ----------
def hex_to_rgba(hex_color, alpha=1.0):
    """Convert an sRGB hex string to a linear-space RGBA tuple.

    Blender's color sockets store linear values; passing raw sRGB floats
    causes a double-gamma wash-out under Standard view transform.
    """
    h = hex_color.lstrip("#")

    def srgb_to_linear(byte):
        c = byte / 255.0
        return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4

    return (
        srgb_to_linear(int(h[0:2], 16)),
        srgb_to_linear(int(h[2:4], 16)),
        srgb_to_linear(int(h[4:6], 16)),
        alpha,
    )


def wipe_scene():
    for obj in list(bpy.data.objects):
        bpy.data.objects.remove(obj, do_unlink=True)
    for block in (bpy.data.meshes, bpy.data.materials, bpy.data.lights, bpy.data.cameras):
        for item in list(block):
            if item.users == 0:
                block.remove(item)


def make_toon_material(name, hex_shadow, hex_mid, hex_light,
                       spec_intensity=0.55, gloss_roughness=0.12):
    """3-band cel-shaded material with subtle glossy spec. Eevee-only.

    `spec_intensity` controls the brightness of the specular highlight (0..1).
    Use a low value (e.g. 0.05) on curved concave surfaces where a sharp spec
    dot reads as an artifact instead of a highlight.
    """
    mat = bpy.data.materials.new(name)
    mat.use_nodes = True
    nodes = mat.node_tree.nodes
    links = mat.node_tree.links

    for n in list(nodes):
        nodes.remove(n)

    output = nodes.new("ShaderNodeOutputMaterial")
    output.location = (900, 0)

    add_shader = nodes.new("ShaderNodeAddShader")
    add_shader.location = (700, 0)

    # ---- Diffuse toon path ----
    emission = nodes.new("ShaderNodeEmission")
    emission.location = (500, 100)
    emission.inputs["Strength"].default_value = 1.0

    ramp = nodes.new("ShaderNodeValToRGB")
    ramp.location = (250, 100)
    ramp.color_ramp.interpolation = "LINEAR"
    ramp.color_ramp.elements[0].position = 0.0
    ramp.color_ramp.elements[0].color = hex_to_rgba(hex_shadow)
    ramp.color_ramp.elements[1].position = 0.45
    ramp.color_ramp.elements[1].color = hex_to_rgba(hex_mid)
    ramp.color_ramp.elements.new(0.80).color = hex_to_rgba(hex_light)

    s2rgb = nodes.new("ShaderNodeShaderToRGB")
    s2rgb.location = (50, 100)

    diffuse = nodes.new("ShaderNodeBsdfDiffuse")
    diffuse.location = (-200, 100)
    diffuse.inputs["Color"].default_value = (1.0, 1.0, 1.0, 1.0)

    ao = nodes.new("ShaderNodeAmbientOcclusion")
    ao.location = (-200, -120)
    ao.inputs["Distance"].default_value = 0.35

    multiply = nodes.new("ShaderNodeMixRGB")
    multiply.location = (160, -50)
    multiply.blend_type = "MULTIPLY"
    multiply.inputs["Fac"].default_value = 1.0

    links.new(diffuse.outputs["BSDF"], s2rgb.inputs["Shader"])
    links.new(s2rgb.outputs["Color"], multiply.inputs["Color1"])
    links.new(ao.outputs["Color"], multiply.inputs["Color2"])
    links.new(multiply.outputs["Color"], ramp.inputs["Fac"])
    links.new(ramp.outputs["Color"], emission.inputs["Color"])

    # ---- Glossy spec path: tight white-hot dot where light hits squarely ----
    spec_emission = nodes.new("ShaderNodeEmission")
    spec_emission.location = (500, -260)
    spec_emission.inputs["Strength"].default_value = 1.0

    spec_ramp = nodes.new("ShaderNodeValToRGB")
    spec_ramp.location = (250, -260)
    spec_ramp.color_ramp.interpolation = "LINEAR"
    spec_ramp.color_ramp.elements[0].position = 0.55
    spec_ramp.color_ramp.elements[0].color = (0.0, 0.0, 0.0, 1.0)
    spec_ramp.color_ramp.elements[1].position = 0.58
    spec_ramp.color_ramp.elements[1].color = (spec_intensity, spec_intensity, spec_intensity, 1.0)

    spec_s2rgb = nodes.new("ShaderNodeShaderToRGB")
    spec_s2rgb.location = (50, -260)

    glossy = nodes.new("ShaderNodeBsdfGlossy")
    glossy.location = (-200, -260)
    glossy.inputs["Color"].default_value = (1.0, 1.0, 1.0, 1.0)
    glossy.inputs["Roughness"].default_value = gloss_roughness

    links.new(glossy.outputs["BSDF"], spec_s2rgb.inputs["Shader"])
    links.new(spec_s2rgb.outputs["Color"], spec_ramp.inputs["Fac"])
    links.new(spec_ramp.outputs["Color"], spec_emission.inputs["Color"])

    # ---- Combine ----
    links.new(emission.outputs["Emission"], add_shader.inputs[0])
    links.new(spec_emission.outputs["Emission"], add_shader.inputs[1])
    links.new(add_shader.outputs["Shader"], output.inputs["Surface"])

    return mat


def make_rounded_prism(name, half_width, height, corner_radius, segments, location, material):
    """Extrude a 2D rounded square in XY by `height` along Z.

    Result has rounded vertical sides; top and bottom remain flat with sharp
    horizontal edges — exactly the "base corners only" rounding shape.
    """
    mesh = bpy.data.meshes.new(name + "_mesh")
    bm = bmesh.new()

    h = half_width
    r = corner_radius
    arcs = [
        (h - r,  h - r,  0.0,             math.pi / 2),
        (-h + r, h - r,  math.pi / 2,     math.pi),
        (-h + r, -h + r, math.pi,         3 * math.pi / 2),
        (h - r,  -h + r, 3 * math.pi / 2, 2 * math.pi),
    ]

    pts = []
    for cx, cy, a0, a1 in arcs:
        for i in range(segments + 1):
            t = i / segments
            ang = a0 + t * (a1 - a0)
            pts.append((cx + r * math.cos(ang), cy + r * math.sin(ang)))

    bm_verts = [bm.verts.new((x, y, 0.0)) for (x, y) in pts]
    bm.faces.new(bm_verts)

    ret = bmesh.ops.extrude_face_region(bm, geom=bm.faces[:])
    new_verts = [v for v in ret["geom"] if isinstance(v, bmesh.types.BMVert)]
    bmesh.ops.translate(bm, verts=new_verts, vec=(0.0, 0.0, height))
    bmesh.ops.recalc_face_normals(bm, faces=bm.faces[:])

    bm.to_mesh(mesh)
    bm.free()

    obj = bpy.data.objects.new(name, mesh)
    bpy.context.collection.objects.link(obj)
    obj.location = location
    obj.data.materials.append(material)
    return obj


def add_boolean(obj, target, name="Boolean", solver="EXACT"):
    mod = obj.modifiers.new(name, "BOOLEAN")
    mod.operation = "DIFFERENCE"
    mod.solver = solver
    mod.object = target
    if hasattr(mod, "use_hole_tolerant"):
        mod.use_hole_tolerant = True
    if hasattr(mod, "use_self"):
        mod.use_self = True
    if hasattr(mod, "material_mode"):
        mod.material_mode = "TRANSFER"
    return mod


def make_cavity_material(name, hex_dark, hex_light, ao_distance=1.4):
    """Direction-independent: AO-driven gradient. Squared AO so concave
    centers (bowl bottom) drop dramatically — gives geometric depth cue
    without relying on normal variation."""
    mat = bpy.data.materials.new(name)
    mat.use_nodes = True
    nodes = mat.node_tree.nodes
    links = mat.node_tree.links

    for n in list(nodes):
        nodes.remove(n)

    output = nodes.new("ShaderNodeOutputMaterial")
    output.location = (400, 0)

    emission = nodes.new("ShaderNodeEmission")
    emission.location = (200, 0)

    ramp = nodes.new("ShaderNodeValToRGB")
    ramp.location = (-50, 0)
    ramp.color_ramp.interpolation = "LINEAR"
    ramp.color_ramp.elements[0].position = 0.05
    ramp.color_ramp.elements[0].color = hex_to_rgba(hex_dark)
    ramp.color_ramp.elements[1].position = 0.85
    ramp.color_ramp.elements[1].color = hex_to_rgba(hex_light)

    ao = nodes.new("ShaderNodeAmbientOcclusion")
    ao.location = (-450, 0)
    ao.inputs["Distance"].default_value = ao_distance

    ao_squared = nodes.new("ShaderNodeMixRGB")
    ao_squared.location = (-220, 0)
    ao_squared.blend_type = "MULTIPLY"
    ao_squared.inputs["Fac"].default_value = 1.0

    links.new(ao.outputs["Color"], ao_squared.inputs["Color1"])
    links.new(ao.outputs["Color"], ao_squared.inputs["Color2"])
    links.new(ao_squared.outputs["Color"], ramp.inputs["Fac"])
    links.new(ramp.outputs["Color"], emission.inputs["Color"])
    links.new(emission.outputs["Emission"], output.inputs["Surface"])

    return mat


# ---------- build ----------
wipe_scene()

mat_magenta = make_toon_material(
    "Magenta", MAGENTA_SHADOW, MAGENTA_MID, MAGENTA_LIGHT,
)
mat_cyan = make_toon_material(
    "Cyan", CYAN_SHADOW, CYAN_MID, CYAN_LIGHT,
    spec_intensity=0.55, gloss_roughness=0.18,
)
# Cavity: AO-driven gradient picking up the darker magenta tones so the
# debossed text reads as "carved into the cyan top" against the magenta
# base underneath.
mat_cavity = make_cavity_material("Cavity", "#1a1230", "#5b3aa0", ao_distance=0.3)

# Bottom layer: bottom face at z=0, top face at z=LAYER_H
bottom = make_rounded_prism(
    "BottomLayer",
    half_width=FOOTPRINT,
    height=LAYER_H,
    corner_radius=CORNER_RADIUS,
    segments=CORNER_SEG,
    location=(0, 0, 0),
    material=mat_magenta,
)

# Top layer: built at the AVERAGE thickness, then top-face verts tilted to
# form a ramp (thin at -Y, thick at +Y).
top = make_rounded_prism(
    "TopLayer",
    half_width=FOOTPRINT,
    height=TOP_H_AVG,
    corner_radius=CORNER_RADIUS,
    segments=CORNER_SEG,
    location=(0, 0, LAYER_H),
    material=mat_cyan,
)
for v in top.data.vertices:
    if v.co.z > TOP_H_AVG / 2:
        v.co.z = TOP_H_AVG + TOP_SLOPE * v.co.y

# Triangulate so each quad's two triangles share a fixed diagonal — without
# this, the now-non-rectangular side quads get inconsistent normals between
# their triangles and toon-shading paints visible facets.
_bm = bmesh.new()
_bm.from_mesh(top.data)
bmesh.ops.triangulate(_bm, faces=_bm.faces[:])
bmesh.ops.recalc_face_normals(_bm, faces=_bm.faces[:])
_bm.to_mesh(top.data)
_bm.free()
top.data.update()

# Text cutters: "." and "fwa" placed as two separate objects so the dot
# can sit tight against the f. Both cutters are tilted by the same ramp
# angle so the deboss tracks the slope at constant depth, not constant
# world-Z.
TEXT_DEPTH = 0.10  # half-thickness of the extruded text mesh

font_path = os.path.join(SCRIPT_DIR, TEXT_FONT)
font = bpy.data.fonts.load(font_path)


def surface_z_at(y):
    """World-z of the inclined top face at the given y (cutters live at y=0)."""
    return LAYER_H + TOP_H_AVG + TOP_SLOPE * y


def make_text_cutter(name, body, x_offset):
    bpy.ops.object.text_add()
    obj = bpy.context.active_object
    obj.name = name
    obj.data.body = body
    obj.data.font = font
    obj.data.size = TEXT_SIZE
    obj.data.extrude = TEXT_DEPTH
    obj.data.align_x = "CENTER"
    obj.data.align_y = "CENTER"
    bpy.context.view_layer.objects.active = obj
    bpy.ops.object.convert(target="MESH")
    bm = bmesh.new()
    bm.from_mesh(obj.data)
    bmesh.ops.remove_doubles(bm, verts=bm.verts[:], dist=0.0001)
    bmesh.ops.recalc_face_normals(bm, faces=bm.faces[:])
    bm.to_mesh(obj.data)
    bm.free()
    obj.location = (x_offset, 0, surface_z_at(0) + TEXT_DEPTH - CAVITY_DEPTH)
    obj.rotation_euler = (TOP_TILT, 0, 0)
    obj.hide_render = True
    obj.data.materials.append(mat_cavity)
    return obj


dot_cutter = make_text_cutter("DotCutter", ".", DOT_X)
fwa_cutter = make_text_cutter("FwaCutter", "fwa", FWA_X)

# Two boolean cuts on top — debosses the dot and "fwa" separately
add_boolean(top, dot_cutter, name="DotBool")
add_boolean(top, fwa_cutter, name="FwaBool")

# Bevel after the booleans so the cavity rim (text edges) gets softened.
top_bevel = top.modifiers.new("EdgeBevel", "BEVEL")
top_bevel.width = TOP_BEVEL_WIDTH
top_bevel.segments = TOP_BEVEL_SEGMENTS
top_bevel.limit_method = "ANGLE"
top_bevel.angle_limit = math.radians(30)

# ---------- camera (orthographic isometric) ----------
bpy.ops.object.camera_add()
cam = bpy.context.active_object
cam.name = "IsoCam"
cam.data.type = "ORTHO"
cam.data.ortho_scale = 2.85
cam.rotation_euler = (math.radians(54.736), 0.0, math.radians(45.0))

view_dir = cam.rotation_euler.to_matrix() @ Vector((0.0, 0.0, 1.0))
target = Vector((0.0, 0.0, LAYER_H))
cam.location = target + view_dir * 8.0
bpy.context.scene.camera = cam

# ---------- lighting ----------
bpy.ops.object.light_add(type="SUN", location=(0.0, 4.0, 5.0))
key = bpy.context.active_object
key.data.energy = 1.5
key.rotation_euler = (math.radians(40), 0.0, 0.0)

bpy.ops.object.light_add(type="AREA", location=(-3.0, 2.5, 3.5))
counter = bpy.context.active_object
counter.data.energy = 20
counter.data.size = 0.75
_to_target = Vector((0.0, 0.0, 0.65)) - counter.location
counter.rotation_euler = _to_target.to_track_quat("-Z", "Y").to_euler()

bpy.ops.object.light_add(type="AREA", location=(4.0, -2.5, 3.5))
fill = bpy.context.active_object
fill.data.energy = 50
fill.data.size = 4.0
fill.rotation_euler = (math.radians(60), math.radians(0), math.radians(150))

bpy.ops.object.light_add(type="AREA", location=(0.0, 0.0, 3.0))
top_fill = bpy.context.active_object
top_fill.data.energy = 55
top_fill.data.size = 2.5
top_fill.rotation_euler = (0.0, 0.0, 0.0)

# Soft world ambient. Tokyo Night bg-dark — close-to-black blue.
world = bpy.data.worlds["World"]
world.use_nodes = True
bg = world.node_tree.nodes["Background"]
bg.inputs["Color"].default_value = hex_to_rgba("#16161e")
bg.inputs["Strength"].default_value = 1.0

# ---------- render ----------
scene = bpy.context.scene

engines = {item.identifier for item in bpy.types.RenderSettings.bl_rna.properties["engine"].enum_items}
for eng in ("BLENDER_EEVEE_NEXT", "BLENDER_EEVEE", "EEVEE"):
    if eng in engines:
        scene.render.engine = eng
        break

if hasattr(scene, "eevee"):
    if hasattr(scene.eevee, "taa_render_samples"):
        scene.eevee.taa_render_samples = 64

scene.render.resolution_x = OUT_W
scene.render.resolution_y = OUT_H
scene.render.film_transparent = True
scene.render.image_settings.file_format = "PNG"
scene.render.image_settings.color_mode = "RGBA"
scene.render.filepath = OUTPUT_PATH

# Toon look: emission-based shaders bypass tone-mapping, so Standard is right
scene.view_settings.view_transform = "Standard"
scene.view_settings.look = "None"

print(f"[build_logo] rendering to {OUTPUT_PATH}")
bpy.ops.render.render(write_still=True)
print("[build_logo] done")
