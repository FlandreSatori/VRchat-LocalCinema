import { createApp } from 'vue'
import { createRouter, createWebHashHistory } from 'vue-router'
import App from './App.vue'
import LibraryPage from './pages/LibraryPage.vue'
import ConvertPage from './pages/ConvertPage.vue'
import ServerPage from './pages/ServerPage.vue'
import EasytierPage from './pages/EasytierPage.vue'
import SettingsPage from './pages/SettingsPage.vue'
import StatisticsPage from './pages/StatisticsPage.vue'
import AboutPage from './pages/AboutPage.vue'
import { initTheme } from './lib/theme'
import './style.css'

initTheme()

const router = createRouter({
  history: createWebHashHistory(),
  routes: [
    { path: '/', redirect: '/library' },
    { path: '/library', component: LibraryPage },
    { path: '/convert', component: ConvertPage },
    { path: '/server', component: ServerPage },
    { path: '/easytier', component: EasytierPage },
    { path: '/settings', component: SettingsPage },
    { path: '/statistics', component: StatisticsPage },
    { path: '/about', component: AboutPage },
  ],
})

createApp(App).use(router).mount('#app')
