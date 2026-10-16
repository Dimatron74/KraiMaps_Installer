/** Роутер установщика: Главная, установка, вход, технический callback. */
import { createRouter, createWebHistory } from 'vue-router';
import AuthGate from '../../features/auth/AuthGate.vue';
import OAuthCallback from '../../features/auth/OAuthCallback.vue';
import HomeView from '../../features/home/HomeView.vue';
import InstallView from '../../features/install/InstallView.vue';

export const routes = [
  { path: '/', name: 'home', component: HomeView },
  { path: '/install', name: 'install', component: InstallView },
  { path: '/enter', name: 'enter', component: AuthGate },
  { path: '/oauth/callback', name: 'oauth-callback', component: OAuthCallback },
];

export const router = createRouter({
  history: createWebHistory(),
  routes,
});
