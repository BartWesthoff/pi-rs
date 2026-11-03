// Deliberately broken: it loads and registers fine, then its session_start handler throws.
export default function (pi) {
	pi.registerCommand("broken-handler", { description: "registered before the failure", handler: async () => {} });
	pi.on("session_start", () => {
		throw new Error("pkg-harness: broken-handler fixture");
	});
}
