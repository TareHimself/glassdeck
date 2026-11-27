import type { InputUpdate, PreviewPass, PreviewTarget } from "./bindings";

/**
 * Draws a canvas the way glassdeck-gpu's `CanvasProgram` does on the keyboard: the same GLSL ES 3.00
 * shaders, each pass into its target and then the canvas, a full-screen triangle each, the `frame`
 * uniforms, and each input as the script last wrote it. Only what changed is uploaded; the frame
 * arrives from the app's side, which runs `keyboard.luau`.
 */

const VERTEX = `#version 300 es
void main() {
    vec2 corner = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    gl_Position = vec4(corner * 2.0 - 1.0, 0.0, 1.0);
}
`;

/** The glue's uniform holding the inputs, as `glassdeck_slang::canvas::INPUTS_UNIFORM` names it. */
const INPUTS_UNIFORM = "gd_inputs";
const PANEL = [1920, 1080] as const;

export type CanvasFrame = { time: number; delta: number; index: number; inputs: InputUpdate[] };
/** A canvas's shaders: the passes drawn first into their targets, then the canvas at `width` by `height`. */
export type CanvasShaders = { fragment: string; width: number; height: number; passes: PreviewPass[]; targets: PreviewTarget[] };

/** One linked shader, a pass's or the canvas's, with its uniforms by their Slang names. */
type Stage = { program: WebGLProgram; uniforms: Map<string, WebGLUniformLocation> };
/** A target's texture, or with feedback the two it swaps between; `shown` is the one drawn last. */
type Surface = { target: PreviewTarget; textures: WebGLTexture[]; framebuffers: WebGLFramebuffer[]; shown: number };

/** The member Slang's std140 wrapper struct keeps an array in. */
const STD140_ARRAY_MEMBER = ".data_0";

/**
 * `gd_inputs_0.roseAt_0` to `gd_inputs.roseAt`, and an array, `gd_inputs_0.bands_0[0]` or as Slang wraps
 * it for std140, `gd_inputs_0.bands_0.data_0[0]`, to `gd_inputs.bands`.
 */
export function slangName(glsl: string): string {
  const array = glsl.endsWith("[0]") ? glsl.slice(0, -"[0]".length) : null;
  const unwrapped = array?.endsWith(STD140_ARRAY_MEMBER) ? array.slice(0, -STD140_ARRAY_MEMBER.length) : array;
  return (unwrapped ?? glsl)
    .split(".")
    .map((part) => part.replace(/_\d+$/, ""))
    .join(".");
}

export class CanvasRenderer {
  private readonly gl: WebGL2RenderingContext;
  private screen: Stage | null = null;
  private passes: { stage: Stage; surface: Surface }[] = [];
  private surfaces: Surface[] = [];
  private pending: CanvasFrame[] = [];
  private scheduled = 0;

  constructor(private readonly canvas: HTMLCanvasElement) {
    // preserveDrawingBuffer keeps the last frame readable, for tests and for a paused canvas
    const gl = canvas.getContext("webgl2", { antialias: false, premultipliedAlpha: false, preserveDrawingBuffer: true });
    if (!gl) throw new Error("this webview has no WebGL2");
    this.gl = gl;
    // rgba16f targets can only be drawn into with this
    gl.getExtension("EXT_color_buffer_float");
    gl.bindVertexArray(gl.createVertexArray());
    // The keyboard scans fragment row 0 out at the top, but a canvas shows it at the bottom.
    canvas.style.transform = "scaleY(-1)";
  }

  /** A new canvas for a new run; inputs start unset until its first frame writes them, and targets black. */
  useProgram(shaders: CanvasShaders) {
    this.release();
    this.surfaces = shaders.targets.map((target) => this.surface(target));
    this.passes = shaders.passes.map((pass) => {
      const surface = this.surfaces.find((s) => s.target.name === pass.writes);
      if (!surface) throw new Error(`a pass writes ${pass.writes}, which isn't a target`);
      return { stage: this.stage(pass.fragment), surface };
    });
    this.screen = this.stage(shaders.fragment);
    this.canvas.width = shaders.width;
    this.canvas.height = shaders.height;
    this.pending = [];
  }

  /** Queues a frame; frames that arrive between two animation frames are drawn as one. */
  frame(frame: CanvasFrame) {
    this.pending.push(frame);
    this.scheduled ||= requestAnimationFrame(() => this.draw());
  }

  clear() {
    this.pending = [];
    this.gl.bindFramebuffer(this.gl.FRAMEBUFFER, null);
    this.gl.clearColor(0, 0, 0, 0);
    this.gl.clear(this.gl.COLOR_BUFFER_BIT);
  }

  dispose() {
    cancelAnimationFrame(this.scheduled);
    this.release();
    this.gl.getExtension("WEBGL_lose_context")?.loseContext();
  }

  private release() {
    const gl = this.gl;
    for (const stage of [this.screen, ...this.passes.map((p) => p.stage)]) if (stage) gl.deleteProgram(stage.program);
    for (const surface of this.surfaces) {
      surface.textures.forEach((texture) => gl.deleteTexture(texture));
      surface.framebuffers.forEach((framebuffer) => gl.deleteFramebuffer(framebuffer));
    }
    this.screen = null;
    this.passes = [];
    this.surfaces = [];
  }

  private stage(fragment: string): Stage {
    const gl = this.gl;
    const program = gl.createProgram();
    for (const [kind, source] of [
      [gl.VERTEX_SHADER, VERTEX],
      [gl.FRAGMENT_SHADER, fragment],
    ] as const) {
      const shader = gl.createShader(kind);
      if (!shader) throw new Error("creating a shader");
      gl.shaderSource(shader, source);
      gl.compileShader(shader);
      if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) throw new Error(`compiling the canvas: ${gl.getShaderInfoLog(shader)}`);
      gl.attachShader(program, shader);
      gl.deleteShader(shader);
    }
    gl.linkProgram(program);
    if (!gl.getProgramParameter(program, gl.LINK_STATUS)) throw new Error(`linking the canvas: ${gl.getProgramInfoLog(program)}`);
    const uniforms = new Map<string, WebGLUniformLocation>();
    const count = gl.getProgramParameter(program, gl.ACTIVE_UNIFORMS) as number;
    for (let i = 0; i < count; i++) {
      const active = gl.getActiveUniform(program, i);
      const location = active && gl.getUniformLocation(program, active.name);
      if (active && location) uniforms.set(slangName(active.name), location);
    }
    return { program, uniforms };
  }

  private surface(target: PreviewTarget): Surface {
    const gl = this.gl;
    const [internal, kind] = target.format === "rgba16f" ? [gl.RGBA16F, gl.HALF_FLOAT] : [gl.RGBA8, gl.UNSIGNED_BYTE];
    const textures: WebGLTexture[] = [];
    const framebuffers: WebGLFramebuffer[] = [];
    for (let i = 0; i < (target.feedback ? 2 : 1); i++) {
      const texture = gl.createTexture();
      gl.bindTexture(gl.TEXTURE_2D, texture);
      gl.texImage2D(gl.TEXTURE_2D, 0, internal, target.width, target.height, 0, gl.RGBA, kind, null);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      const framebuffer = gl.createFramebuffer();
      gl.bindFramebuffer(gl.FRAMEBUFFER, framebuffer);
      gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, texture, 0);
      if (gl.checkFramebufferStatus(gl.FRAMEBUFFER) !== gl.FRAMEBUFFER_COMPLETE) {
        throw new Error(`this preview can't draw into target ${target.name} (${target.format})`);
      }
      gl.viewport(0, 0, target.width, target.height);
      gl.clearColor(0, 0, 0, 0);
      gl.clear(gl.COLOR_BUFFER_BIT);
      textures.push(texture);
      framebuffers.push(framebuffer);
    }
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    return { target, textures, framebuffers, shown: 0 };
  }

  private draw() {
    this.scheduled = 0;
    const frames = this.pending;
    this.pending = [];
    const latest = frames.at(-1);
    if (!this.screen || !latest) return;
    const inputs = frames.flatMap((f) => f.inputs);
    for (const { stage, surface } of this.passes) {
      const drawing = (surface.shown + 1) % surface.textures.length;
      this.drawStage(stage, surface.framebuffers[drawing] ?? null, surface.target.width, surface.target.height, latest, inputs);
      surface.shown = drawing;
    }
    this.drawStage(this.screen, null, this.canvas.width, this.canvas.height, latest, inputs);
  }

  /** One full-screen draw, with every target bound to the texture unit of its place in the list. */
  private drawStage(stage: Stage, framebuffer: WebGLFramebuffer | null, width: number, height: number, frame: CanvasFrame, inputs: InputUpdate[]) {
    const gl = this.gl;
    const uniform = (name: string) => stage.uniforms.get(name) ?? null;
    gl.bindFramebuffer(gl.FRAMEBUFFER, framebuffer);
    gl.viewport(0, 0, width, height);
    gl.useProgram(stage.program);
    gl.uniform1f(uniform("frame.time"), frame.time);
    gl.uniform1f(uniform("frame.delta"), frame.delta);
    gl.uniform1ui(uniform("frame.index"), frame.index);
    gl.uniform2f(uniform("frame.resolution"), width, height);
    gl.uniform2f(uniform("frame.panel"), PANEL[0], PANEL[1]);
    this.surfaces.forEach((surface, unit) => {
      gl.activeTexture(gl.TEXTURE0 + unit);
      gl.bindTexture(gl.TEXTURE_2D, surface.textures[surface.shown] ?? null);
      gl.uniform1i(uniform(surface.target.name), unit);
    });
    for (const { name, value } of inputs) {
      const location = uniform(`${INPUTS_UNIFORM}.${name}`);
      switch (value.kind) {
        case "float": {
          const [x = 0, y = 0, z = 0, w = 0] = value.values.map((v) => v ?? 0);
          if (value.values.length === 1) gl.uniform1f(location, x);
          else if (value.values.length === 2) gl.uniform2f(location, x, y);
          else if (value.values.length === 3) gl.uniform3f(location, x, y, z);
          else gl.uniform4f(location, x, y, z, w);
          break;
        }
        case "uint":
          gl.uniform1ui(location, value.value);
          break;
        case "array":
          gl.uniform1fv(location, value.values.map((v) => v ?? 0));
          break;
      }
    }
    gl.drawArrays(gl.TRIANGLES, 0, 3);
  }
}
