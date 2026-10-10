/* @refresh reload */
import { render } from "solid-js/web";
import "./styles.css";
import "./features.css";
import App from "./App";

render(() => <App />, document.getElementById("root") as HTMLElement);
