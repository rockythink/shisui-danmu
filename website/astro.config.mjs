import { defineConfig } from "astro/config";
import starlight from "@astrojs/starlight";

export default defineConfig({
  site: "https://danmu.elazer.wang",
  output: "static",
  build: {
    format: "directory",
  },
  integrations: [
    starlight({
      title: "DANMU",
      description: "DANMU v0.5.0 中文使用手册：安装、直播间操作、OBS、AI 助手、审核发送、数据隐私与故障排查。",
      favicon: "/favicon.svg",
      customCss: ["./src/styles/starlight.css"],
      locales: {
        root: { label: "简体中文", lang: "zh-CN" },
      },
      social: [
        { icon: "github", label: "GitHub", href: "https://github.com/rockythink/shisui-danmu" },
      ],
      sidebar: [
        { label: "使用手册", link: "/guide/" },
      ],
    }),
  ],
  vite: {
    build: {
      cssMinify: "lightningcss",
    },
  },
});
