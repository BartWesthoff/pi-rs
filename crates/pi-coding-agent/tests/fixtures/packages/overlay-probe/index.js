// Opens a capturing overlay and returns without waiting for it, so a later tool call (an
// `ask_user_question`) finds another overlay already on screen. Escape closes the overlay.
export default function (pi) {
	pi.registerTool({
		name: "open_overlay_probe",
		label: "Overlay probe",
		description: "Package harness: open a focused overlay and leave it open.",
		parameters: { type: "object", properties: {} },
		execute: async (_toolCallId, _params, _signal, _onUpdate, ctx) => {
			void ctx.ui.custom(
				(_tui, _theme, _keybindings, done) => ({
					render: (width) => ["OVERLAY PROBE OPEN".padEnd(Math.min(width, 40)), "esc closes the probe"],
					handleInput: (data) => {
						if (data === "\x1b") done("closed");
					},
					invalidate: () => {},
				}),
				{ overlay: true, overlayOptions: { anchor: "center", width: 40 } },
			);
			return { content: [{ type: "text", text: "overlay opened" }], details: {} };
		},
	});
}
