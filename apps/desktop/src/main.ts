import { createApp } from "vue";
import App from "./App.vue";
import { choice, setLocale } from "./i18n";
import "./style.css";

setLocale(choice.value);
createApp(App).mount("#app");
