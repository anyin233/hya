import { defineTuiExtension } from "../../src";

export default defineTuiExtension({
  activate(api) {
    api.registerPanel({ id: "spin", title: "Spin", render: () => { for (;;) { /* until the VM deadline */ } } });
    api.registerPanel({ id: "fast", title: "Fast", render: () => "fast" });
    api.registerPanel({ id: "read", title: "Read", render: async () => `read:${await api.fs.read("notes.md")}` });
  },
});
