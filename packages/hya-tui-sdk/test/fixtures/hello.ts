import { defineTuiExtension } from "../../src";

export default defineTuiExtension({
  activate(api) {
    console.log("activated");
    api.registerPanel({ id: "hello", title: "Hello", render: ({ context }) => `hello ${context.git?.branch ?? "?"}` });
  },
});
