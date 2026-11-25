import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "@/App";
import { ThemeProvider } from "@/lib/theme";
import { installNativeFeel } from "@/lib/native-feel";
import { UpdatesProvider } from "@/lib/updates";
import { Pill } from "@/screens/Pill";
import "@/index.css";

installNativeFeel();

// The floating recorder pill is a second window loading this bundle with ?view=pill.
const isPill = new URLSearchParams(window.location.search).get("view") === "pill";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ThemeProvider>
      {isPill ? (
        <Pill />
      ) : (
        <UpdatesProvider>
          <App />
        </UpdatesProvider>
      )}
    </ThemeProvider>
  </StrictMode>,
);
