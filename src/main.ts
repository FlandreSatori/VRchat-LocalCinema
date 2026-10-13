import { createApp } from 'vue'
import { createRouter, createWebHashHistory } from 'vue-router'
import App from './App.vue'
import LibraryPage from './pages/LibraryPage.vue'
import ServerPage from './pages/ServerPage.vue'
import SettingsPage from './pages/SettingsPage.vue'
import AboutPage from './pages/AboutPage.vue'
import { initTheme } from './lib/theme'
import './style.css'

initTheme()

const router = createRouter({
  history: createWebHashHistory(),
  routes: [
    { path: '/', redirect: '/library' },
    { path: '/library', component: LibraryPage },
    { path: '/server', component: ServerPage },
    { path: '/settings', component: SettingsPage },
    { path: '/about', component: AboutPage },
  ],
})

createApp(App).use(router).mount('#app')
