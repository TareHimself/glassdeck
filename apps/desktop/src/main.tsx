import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { createBrowserRouter, RouterProvider } from "react-router";
import { Layout } from "./Layout";
import { followGlassdeckd } from "./glassdeckd";
import { Backgrounds } from "./screens/Backgrounds";
import { NewWidget } from "./screens/NewWidget";
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
      { path: "widgets/new", element: <NewWidget /> },
      { path: "widgets/:id", element: <WidgetEditor /> },
      { path: "backgrounds", element: <Backgrounds /> },
    ],
  },
]);

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
