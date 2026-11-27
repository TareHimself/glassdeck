import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { createBrowserRouter, RouterProvider } from "react-router";
import { BareLayout, Layout } from "./Layout";
import { followGlassdeckd } from "./glassdeckd";
import { followKeyboard } from "./keyboard";
import { BackgroundEditor } from "./screens/BackgroundEditor";
import { Backgrounds, ChooseBackground } from "./screens/Backgrounds";
import { Keyboard } from "./screens/Keyboard";
import { Overview } from "./screens/Overview";
import { WidgetEditor } from "./screens/WidgetEditor";
import { AddWidget, Widgets } from "./screens/Widgets";
import "@fontsource-variable/inter";
import "@fontsource-variable/jetbrains-mono";
import "./monaco";
import "./theme.css";

const router = createBrowserRouter([
  {
    element: <Layout />,
    children: [
      { index: true, element: <Overview /> },
      { path: "widgets", element: <Widgets /> },
      { path: "widgets/:id", element: <WidgetEditor /> },
      { path: "backgrounds", element: <Backgrounds /> },
      { path: "backgrounds/:id", element: <BackgroundEditor /> },
      { path: "keyboard", element: <Keyboard /> },
    ],
  },
  {
    element: <BareLayout />,
    children: [
      { path: "choose/background", element: <ChooseBackground /> },
      { path: "choose/widget", element: <AddWidget /> },
    ],
  },
]);

if (import.meta.env.MODE === "mock") {
  const { installMockBackend } = await import("./mock/backend");
  installMockBackend();
}

followGlassdeckd().catch((error: unknown) => {
  console.error("following glassdeckd", error);
});

followKeyboard().catch((error: unknown) => {
  console.error("following the keyboard", error);
});

const root = document.getElementById("root");
if (!root) {
  throw new Error("index.html has no #root");
}

createRoot(root).render(
  <StrictMode>
    <RouterProvider router={router} />
  </StrictMode>,
);
