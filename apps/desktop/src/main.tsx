import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { createBrowserRouter, RouterProvider } from "react-router";
import { Layout } from "./Layout";
import { followGlassdeckd } from "./glassdeckd";
import { BackgroundEditor } from "./screens/BackgroundEditor";
import { Backgrounds } from "./screens/Backgrounds";
import { Overview } from "./screens/Overview";
import { WidgetEditor } from "./screens/WidgetEditor";
import { Widgets } from "./screens/Widgets";
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

const root = document.getElementById("root");
if (!root) {
  throw new Error("index.html has no #root");
}

createRoot(root).render(
  <StrictMode>
    <RouterProvider router={router} />
  </StrictMode>,
);
