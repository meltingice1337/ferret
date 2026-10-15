/* @refresh reload */
import { render } from "solid-js/web";
import "./styles.css";
import "./features.css";
import "./media.css";
import App from "./App";
import { installErrorHandlers } from "./lib/log";
import "./lib/testhooks";

installErrorHandlers();

render(() => <App />, document.getElementById("root") as HTMLElement);
