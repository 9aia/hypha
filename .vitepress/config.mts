import { defineConfig } from "vitepress";
import llmstxt from "vitepress-plugin-llms";

export default defineConfig({
  vite: {
    plugins: [llmstxt()],
  },
  title: "hypha",
  description:
    "the distributed substrate for agent workloads — entities, ops, snapshots, and locks across your machines",
  base: "/hypha/",
  ignoreDeadLinks: [/\/api\//],
  themeConfig: {
    nav: [
      { text: "Getting Started", link: "/getting-started" },
      { text: "Tutorial", link: "/tutorials/first-entity" },
      { text: "How-to", link: "/how-to/run-the-tests" },
      { text: "Explanation", link: "/explanation/architecture" },
      { text: "Reference", link: "/reference/crates" },
      {
        text: "LLMs",
        items: [
          { text: "llms.txt", link: "https://9aia.com/hypha/llms.txt", target: "_blank" },
          { text: "llms-full.txt", link: "https://9aia.com/hypha/llms-full.txt", target: "_blank" },
        ],
      },
      { text: "9aia", link: "https://9aia.com" },
    ],
    sidebar: [
      {
        text: "Tutorials",
        items: [{ text: "Your first entity", link: "/tutorials/first-entity" }],
      },
      {
        text: "How-to",
        items: [
          { text: "Build and test", link: "/how-to/run-the-tests" },
          { text: "Read the ledger", link: "/how-to/read-the-ledger" },
        ],
      },
      {
        text: "Explanation",
        items: [
          { text: "Architecture", link: "/explanation/architecture" },
          { text: "Entities, ops, snapshots", link: "/explanation/model" },
          { text: "Boundary with sepia and peregrin", link: "/explanation/boundary" },
        ],
      },
      {
        text: "Reference",
        items: [
          { text: "Crates", link: "/reference/crates" },
          { text: "Environment", link: "/reference/environment" },
          { text: "Wire API", link: "/reference/wire-api" },
        ],
      },
    ],
    socialLinks: [{ icon: "github", link: "https://github.com/9aia/hypha" }],
    footer: {
      message: "MIT licensed — part of the 9aia GAIA stack.",
      copyright: "9aia",
    },
  },
});
