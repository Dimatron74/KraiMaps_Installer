<script setup lang="ts">
import { onMounted } from 'vue';
import { useRouter } from 'vue-router';
import { useAuthStore } from '../../app/stores/auth';
import { useUpdaterStore } from '../../app/stores/updater';
import { SOCIAL_LINKS, KRAIMAPS_SITE_URL } from '../../shared/config';
import Alert from '../../shared/ui/Alert.vue';
import Btn from '../../shared/ui/Btn.vue';
import tgRaw from '../../assets/telegram-svgrepo-com.svg?raw';
import vkRaw from '../../assets/vk-svgrepo-com.svg?raw';

const auth = useAuthStore();
const router = useRouter();
const updater = useUpdaterStore();

/** Иконки из assets: в файлах зашит чёрный цвет — приводим к цвету кнопки. */
const tgSvg = tgRaw.replace(/#0F0F0F/g, 'currentColor');
const vkSvg = vkRaw.replace(/#020202/g, 'currentColor');

onMounted(() => {
  if (auth.status === 'unknown') void auth.refresh();
  // Версию грузит App.vue при старте (нужна и deep-link путям мимо главной).
});

function goInstall() {
  router.push('/install');
}

async function login() {
  await auth.startLogin(auth.pendingMapId);
}

async function openSocial(url: string) {
  const { openUrl } = await import('@tauri-apps/plugin-opener');
  await openUrl(url);
}
</script>

<template>
  <div class="relative flex min-h-full flex-1 flex-col overflow-hidden">
    <!-- Сцена на весь экран главной: небо → глубина -->
    <div
      class="absolute inset-0"
      aria-hidden="true"
      style="
        background:
          radial-gradient(600px 260px at 78% 8%, rgba(74, 222, 128, 0.14), transparent 65%),
          radial-gradient(700px 320px at 18% 0%, rgba(56, 189, 248, 0.16), transparent 65%),
          linear-gradient(to bottom, #0b1526 0%, #0a1220 45%, #050810 100%);
      "
    />
    <!-- Сетка кубов -->
    <div
      class="absolute inset-0 opacity-[0.13]"
      aria-hidden="true"
      style="
        background-image:
          repeating-linear-gradient(to right, rgba(148, 197, 255, 0.5) 0 1px, transparent 1px 44px),
          repeating-linear-gradient(to bottom, rgba(148, 197, 255, 0.5) 0 1px, transparent 1px 44px);
        mask-image: radial-gradient(560px 380px at 50% 30%, black 30%, transparent 75%);
      "
    />
    <!-- Парящие блоки: тропа справа + тихий противовес слева -->
    <div class="absolute right-[10%] top-[12%] hidden h-16 w-16 sm:block" aria-hidden="true">
      <div class="h-full w-full rounded-lg border border-emerald-300/30 bg-emerald-400/10 shadow-[0_0_40px_rgba(74,222,128,0.25)]" />
    </div>
    <div class="absolute right-[17%] top-[34%] hidden h-9 w-9 sm:block" aria-hidden="true">
      <div class="h-full w-full rounded-md border border-sky-300/30 bg-sky-400/10 shadow-[0_0_28px_rgba(56,189,248,0.3)]" />
    </div>
    <div class="absolute right-[11%] top-[52%] hidden h-5 w-5 sm:block" aria-hidden="true">
      <div class="h-full w-full rounded border border-white/20 bg-white/[0.05]" />
    </div>
    <div class="absolute bottom-[16%] left-[7%] hidden h-6 w-6 opacity-60 sm:block" aria-hidden="true">
      <div class="h-full w-full rounded border border-white/15 bg-white/[0.03]" />
    </div>

    <!-- Контент -->
    <div class="relative mx-auto flex w-full max-w-2xl flex-1 flex-col justify-center px-6 py-12 sm:px-10">
      <!-- Подсказка после «Установить без авторизации» -->
      <Alert v-if="auth.pendingMapId !== null && !auth.isAuthed" tone="info" class="mb-6 text-left">
        <p class="text-sm font-semibold text-primary">Установка без входа</p>
        <p class="mt-1 text-xs text-secondary">
          С сайта напрямую ставить можно только после входа. Скачай архив на странице карты
          и выбери его кнопкой ниже.
        </p>
      </Alert>
        <p class="text-xs font-semibold uppercase tracking-[0.2em] text-emerald-300/80">
          Установщик карт
        </p>
        <h2 class="mt-3 max-w-md text-3xl font-extrabold leading-tight text-primary sm:text-4xl">
          Карты КрайКарт —<br />в твою игру
        </h2>
        <p class="mt-3 max-w-md text-sm leading-relaxed text-secondary">
          Установщик сам установит карту в твой лаунчер.<br />
          С компьютера - файлом, с сайта - в один клик со страницы карты.
        </p>
        <div class="mt-7 flex flex-wrap items-center gap-3">
          <Btn size="lg" @click="goInstall">Выбрать файл с компьютера</Btn>
          <Btn
            v-if="!auth.isAuthed"
            variant="secondary"
            size="lg"
            :loading="auth.status === 'authorizing'"
            @click="login"
          >
            Войти и ставить в 1 клик
          </Btn>
        </div>
        <p v-if="auth.isAuthed" class="mt-4 max-w-md text-xs leading-relaxed text-tertiary">
          Авторизован. Открой страницу карты на сайте и нажми «Установить».
          Установщик откроется сам.
        </p>

        <!-- Польза входа — только гостям -->
        <ul v-if="!auth.isAuthed" class="mt-8 flex max-w-lg flex-col gap-2.5 sm:flex-row sm:gap-6">
          <li class="flex items-center gap-2 text-xs text-secondary">
            <svg class="h-4 w-4 shrink-0 text-emerald-300" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path stroke-linecap="round" stroke-linejoin="round" d="M13 10V3L4 14h7v7l9-11h-7z" /></svg>
            В один клик
          </li>
          <li class="flex items-center gap-2 text-xs text-secondary">
            <svg class="h-4 w-4 shrink-0 text-emerald-300" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path stroke-linecap="round" stroke-linejoin="round" d="M12 8v4l3 3m6-3a9 9 0 1 1-18 0 9 9 0 0 1 18 0z" /></svg>
            История в профиле
          </li>
          <li class="flex items-center gap-2 text-xs text-secondary">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="h-4 w-4 shrink-0 text-emerald-300"><path d="m15.477 12.89 1.515 8.526a.5.5 0 0 1-.81.47l-3.58-2.687a1 1 0 0 0-1.197 0l-3.586 2.686a.5.5 0 0 1-.81-.469l1.514-8.526"/><circle cx="12" cy="8" r="6"/></svg>
            Зачёт для наград
          </li>
        </ul>

        <!-- Соцсети -->
        <section class="flex items-center gap-2 pt-8">
      <button
        v-for="s in SOCIAL_LINKS"
        :key="s.id"
        type="button"
        :title="s.label"
        :aria-label="s.label"
        class="flex h-10 w-10 items-center justify-center rounded-xl border border-white/10 bg-white/[0.03] text-secondary transition-colors hover:border-primary-500/40 hover:text-primary"
        @click="openSocial(s.url)"
      >
        <svg v-if="s.id === 'site'" class="h-5 w-5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true">
          <circle cx="12" cy="12" r="10" />
          <path d="M2 12h20M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z" />
        </svg>
        <span
          v-else-if="s.id === 'telegram'"
          class="block h-5 w-5 [&_svg]:block [&_svg]:h-full [&_svg]:w-full"
          aria-hidden="true"
          v-html="tgSvg"
        />
        <span
          v-else
          class="block h-5 w-5 [&_svg]:block [&_svg]:h-full [&_svg]:w-full"
          aria-hidden="true"
          v-html="vkSvg"
        />
      </button>
      <a
        class="ml-1 text-xs text-tertiary hover:text-primary"
        :href="KRAIMAPS_SITE_URL + '/installer'"
        @click.prevent="openSocial(KRAIMAPS_SITE_URL + '/installer')"
      >
        Об установщике
      </a>
        </section>

        <!-- Обновления: версия + ручная проверка (тихая — при старте). -->
        <section class="flex flex-wrap items-center gap-x-3 gap-y-1 pt-4" aria-label="Обновления">
          <span class="font-mono text-[11px] text-tertiary">v{{ updater.version }}</span>
          <button
            type="button"
            class="cursor-pointer text-[11px] text-tertiary underline decoration-dotted underline-offset-2 hover:text-primary disabled:opacity-50"
            :disabled="updater.phase === 'checking' || updater.phase === 'downloading'"
            @click="updater.checkManual()"
          >
            {{ updater.phase === 'checking' ? 'Проверяем…' : 'Проверить обновления' }}
          </button>
          <span v-if="updater.manualMessage" class="text-[11px] text-tertiary">
            {{ updater.manualMessage }}
          </span>
        </section>
    </div>
  </div>
</template>
