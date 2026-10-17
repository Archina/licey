/**
 * Headless viewport smoke test for the bevy port.
 *
 * Wraps `scripts/viewport-smoke.sh`: builds (optionally), launches mslicer,
 * screenshots the window, crops the viewport, and reports what the model adds
 * over an empty scene plus any panic/wgpu validation errors. This is the
 * verification loop used throughout the bevy migration, where there is no
 * synthetic input tooling and the only way to observe the 3D view is pixels.
 */
import type { CustomToolFactory } from "@oh-my-pi/pi-coding-agent";

const factory: CustomToolFactory = (pi) => ({
	name: "mslicer_viewport",
	label: "mslicer viewport",
	description:
		"Build and launch mslicer headlessly, screenshot the 3D viewport, and report the model's visible bounding box plus any panic or wgpu validation errors. Use after changing rendering, scene sync, camera or gizmo code.",
	parameters: pi.zod.object({
		model: pi.zod
			.string()
			.optional()
			.describe("Mesh to open, relative to the repo root (default: mslicer/assets/teapot.stl)"),
		compareEmpty: pi.zod
			.boolean()
			.optional()
			.describe("Also capture an empty scene and diff (default: true)"),
		build: pi.zod.boolean().optional().describe("Run cargo build first (default: true)"),
		settle: pi.zod.number().optional().describe("Seconds to wait before screenshotting (default: 10)"),
		crop: pi.zod
			.string()
			.optional()
			.describe("ImageMagick crop WxH+X+Y of the window content area"),
		out: pi.zod.string().optional().describe("Screenshot path (default: target/viewport-smoke/viewport.png)"),
	}),

	async execute(_toolCallId, params, onUpdate, _ctx, signal) {
		const root = await pi.exec("git", ["rev-parse", "--show-toplevel"], { cwd: pi.cwd, signal });
		if (root.code !== 0) {
			throw new Error(`not a git repository: ${root.stderr.trim()}`);
		}
		const repo = root.stdout.trim();
		const script = `${repo}/scripts/viewport-smoke.sh`;

		const args: string[] = [];
		if (params.model) args.push("--model", params.model);
		if (params.compareEmpty !== false) args.push("--compare-empty");
		if (params.build === false) args.push("--no-build");
		if (params.settle !== undefined) args.push("--settle", String(params.settle));
		if (params.crop) args.push("--crop", params.crop);
		if (params.out) args.push("--out", params.out);

		onUpdate?.({
			content: [{ type: "text", text: `Running ${script} ...` }],
			details: { phase: "running" },
		});

		const result = await pi.exec(script, args, { cwd: repo, signal });
		const output = `${result.stdout}${result.stderr}`.trim();
		if (result.killed) {
			throw new Error("viewport smoke test was cancelled");
		}

		const bbox = output.match(/bbox=(\S+)\s+px=(\d+)/);
		const probes = output.match(/probes:\s+(.*)$/m);
		const logLine = output.match(/^(?:\d+:)?\s*(.*Validation Error.*)$/m);
		const shot = output.match(/screenshot:\s+(\S+)/);
		const failed = result.code !== 0;

		const summary = [
			failed ? `FAILED (exit ${result.code})` : "passed",
			bbox ? `model bbox ${bbox[1]} (${bbox[2]} px differ from an empty scene)` : undefined,
			probes ? `viewport probes ${probes[1]}` : undefined,
			logLine ? `logged: ${logLine[1]}` : "no panics or validation errors",
			shot ? `screenshot ${shot[1]}` : undefined,
		]
			.filter(Boolean)
			.join("\n");

		return {
			content: [{ type: "text", text: `${summary}\n\n--- script output ---\n${output}` }],
			details: {
				exitCode: result.code,
				failed,
				bbox: bbox?.[1] ?? null,
				changedPixels: bbox ? Number(bbox[2]) : null,
				screenshot: shot?.[1] ?? null,
				output,
			},
		};
	},
});

export default factory;
