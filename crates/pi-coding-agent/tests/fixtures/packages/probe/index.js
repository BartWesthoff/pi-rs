// A well-behaved package: one tool, one command, one skill (from package.json).
export default function (pi) {
	pi.registerTool({
		name: "probe_tool",
		label: "Probe",
		description: "Package harness probe tool.",
		parameters: { type: "object", properties: {} },
		execute: async () => ({ content: [{ type: "text", text: "probe" }], details: {} }),
	});
	pi.registerCommand("probe-command", { description: "Package harness probe command.", handler: async () => {} });
}
