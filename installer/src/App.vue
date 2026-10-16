<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue';
import { RouterView, useRoute, useRouter } from 'vue-router';
import { useAuthStore } from './app/stores/auth';
import { useSiteInstallStore } from './app/stores/siteInstall';
import { useUpdaterStore } from './app/stores/updater';
import UpdateBlocked from './features/updater/UpdateBlocked.vue';
import UpdateOffer from './features/updater/UpdateOffer.vue';
import { parseProtocolUrl } from './shared/utils/protocol';
import { resolveMediaUrl } from './shared/utils/media';
import Alert from './shared/ui/Alert.vue';
import BrandMark from './shared/ui/BrandMark.vue';
import ErrorDetails from './shared/ui/ErrorDetails.vue';
import Skeleton from './shared/ui/Skeleton.vue';

const auth = useAuthStore();
const site = useSiteInstallStore();
const updater = useUpdaterStore();
const router = useRouter();
const route = useRoute();

/**
 * Аватар шапки как на сайте (ProfileAvatar):
 * бэкенд отдаёт S3-ключ — полный URL собирает resolveMediaUrl.
 * computed реактивен: после входа/refresh картинка подгружается сама,
 * без перезапуска. Битая картинка → fallback с первой буквой ника.
 */
const avatarSrc = computed(() => resolveMediaUrl(auth.user?.avatar_url));
const avatarFailed = ref(false);
watch(avatarSrc, () => {
  avatarFailed.value = false;
});
const showAvatar = computed(() => avatarSrc.value !== null && !avatarFailed.value);

async function handleRawUrl(raw: string) {
  const target = parseProtocolUrl(raw);
  if (target.kind === 'oauth') {
    try {
      await auth.handleOAuthCallback(target.code, target.state);
      if (auth.pendingMapId !== null) {
        const id = auth.pendingMapId;
        const mode = auth.pendingMode;
        auth.pendingMapId = null;
        // Сначала уходим с экрана входа на прогресс, потом качаем:
        // иначе пользователь сидит на логине без обратной связи.
        router.push('/install');
        await site.startFromSite(id, mode, 'Вход выполнен');
      } else {
        router.push('/');
      }
    } catch (e) {
      // handleOAuthCallback уже решил: guest + ошибка (фатально) или
      // сохранённый статус + транзиент (сеть легла посреди входа).
      // Транзиент не затираем гостем и на экран входа не уводим (I-3):
      // баннер «нет соединения» с повтором виден на текущей странице.
      if (auth.hasConnectionIssue) return;
      // Ошибку не глотаем: handleOAuthCallback уже записал текст+код+детали,
      // AuthGate покажет их через «Подробнее».
      auth.status = 'guest';
      auth.setError(e);
      router.push({
        name: 'enter',
        query: auth.pendingMapId !== null ? { mapId: String(auth.pendingMapId) } : {},
      });
    }
    return;
  }
  if (target.kind === 'install') {
    if (auth.status === 'unknown') await auth.refresh();
    if (!auth.isAuthed) {
      auth.pendingMapId = target.mapId;
      auth.pendingMode = target.mode;
      router.push({ name: 'enter', query: { mapId: String(target.mapId) } });
      return;
    }
    // Прогресс виден сразу: переход до долгого скачивания.
    router.push('/install');
    await site.startFromSite(target.mapId, target.mode);
  }
}

async function retryConnection() {
  await auth.retry();
}

onMounted(async () => {
  // Автовозврат при появлении сети (I-3): статус вернётся сам, без ручного входа.
  window.addEventListener('online', () => {
    void auth.retry();
  });
  await auth.refresh();
  // Версия для экранов обновлений грузится здесь (а не на главной):
  // deep-link может открыть сразу /install, минуя главную.
  // loadVersion не бросает (внутри try/catch с fallback).
  await updater.loadVersion();
  // Тихая проверка обновлений: fail-open — офлайн молчим и работаем дальше.
  void updater.checkAuto();
  try {
    const { onOpenUrl } = await import('@tauri-apps/plugin-deep-link');
    await onOpenUrl((urls) => {
      for (const u of urls) void handleRawUrl(u);
    });
    const { getCurrent } = await import('@tauri-apps/plugin-deep-link');
    const initial = await getCurrent();
    for (const u of initial ?? []) await handleRawUrl(u);
  } catch {
    // Браузерный dev (pnpm dev без Tauri): deep-link недоступен — обычные роуты.
  }
});

async function logout() {
  await auth.logout();
  router.push('/');
}
</script>

<template>
  <div class="relative flex h-full flex-col overflow-hidden bg-surface">
    <div class="pointer-events-none absolute inset-0" aria-hidden="true">
      <div
        class="absolute inset-0"
        style="
          background:
            radial-gradient(1100px 380px at 50% -140px, rgba(56, 189, 248, 0.07), transparent 70%),
            linear-gradient(to bottom, rgba(15, 22, 38, 0.55), transparent 340px);
        "
      />
    </div>

    <header class="relative border-b border-white/5 px-6 py-3">
      <div class="mx-auto flex max-w-2xl items-center gap-3">
        <button type="button" class="flex cursor-pointer items-center gap-3" @click="router.push('/')">
          <BrandMark size="lg" />
          <span class="text-left">
            <span class="block text-sm font-bold text-primary">Установщик карт КрайКарт</span>
            <span class="block text-[11px] text-tertiary">Установка карты в игру за пару кликов</span>
          </span>
        </button>
        <div class="ml-auto flex items-center gap-2">
          <template v-if="auth.isAuthed && auth.user">
            <img
              v-if="showAvatar"
              :src="avatarSrc ?? undefined"
              alt="Аватар"
              class="h-8 w-8 rounded-full border border-white/10 object-cover"
              @error="avatarFailed = true"
            />
            <span
              v-else
              class="flex h-8 w-8 items-center justify-center rounded-full border border-white/10 bg-primary-500/15 text-xs font-bold text-primary-300"
              aria-hidden="true"
            >
              {{ auth.user.username.slice(0, 1).toUpperCase() }}
            </span>
            <span class="hidden max-w-32 truncate text-xs font-semibold text-primary sm:inline">
              {{ auth.user.username }}
            </span>
            <button
              type="button"
              class="cursor-pointer rounded-lg border border-white/10 px-2.5 py-1.5 text-xs text-secondary hover:border-primary-500/40 hover:text-primary"
              @click="logout"
            >
              Выйти
            </button>
          </template>
          <template v-else-if="auth.status !== 'unknown'">
            <button
              type="button"
              class="cursor-pointer rounded-lg border border-white/10 px-2.5 py-1.5 text-xs text-secondary hover:border-primary-500/40 hover:text-primary"
              @click="router.push('/enter')"
            >
              Войти
            </button>
          </template>
          <template v-else>
            <!-- Сессия ещё проверяется: скелетон вместо прыжка «гость → юзер». -->
            <Skeleton width="2rem" height="2rem" rounded="9999px" />
            <Skeleton width="4.5rem" height="1.75rem" rounded="0.5rem" />
          </template>
        </div>
      </div>
    </header>

    <main
      class="relative flex flex-1 overflow-y-auto"
      :class="route.name === 'home' && auth.status !== 'unknown' ? '' : 'px-6 py-6'"
    >
      <!-- Транзиент (I-3): «нет соединения» вместо экрана входа + предупреждение
        хранилища (I-4.2). Оверлеем, чтобы не ломать раскладку страниц. -->
      <div
        v-if="auth.hasConnectionIssue || auth.storePersistent === false"
        class="absolute inset-x-0 top-0 z-10 mx-auto w-full max-w-2xl space-y-2 px-6 pt-3"
      >
        <Alert v-if="auth.hasConnectionIssue" tone="warning">
          <div class="flex items-center gap-3 text-left">
            <p class="flex-1 text-xs text-secondary">
              <span class="block text-sm font-semibold text-primary">Нет соединения с сайтом</span>
              Сессия сохранена, вход не нужен.
            </p>
            <button
              type="button"
              class="shrink-0 cursor-pointer rounded-lg border border-white/10 px-2.5 py-1.5 text-xs text-secondary hover:border-primary-500/40 hover:text-primary"
              @click="retryConnection"
            >
              Повторить
            </button>
          </div>
          <ErrorDetails
            class="mt-3 text-left"
            :code="auth.transientCode ?? 'E_UNKNOWN'"
            message="Нет соединения с сайтом"
            :details="auth.transientDetails ?? ''"
            hint="Сессия сохранена, вход не нужен. Проверь интернет и нажми «Повторить»."
          />
        </Alert>
        <Alert v-if="auth.storePersistent === false" tone="warning">
          <p class="text-left text-xs text-secondary">
            <span class="block text-sm font-semibold text-primary">Сессия может не сохраниться</span>
            Хранилище ключей недоступно (на Linux нужна служба Secret Service: gnome-keyring
            или KWallet). После перезапуска, возможно, придётся войти снова.
          </p>
        </Alert>
      </div>
      <!-- Пока сессия проверяется — скелетон вместо гостевого контента. -->
      <div
        v-if="auth.status === 'unknown'"
        class="mx-auto flex w-full max-w-2xl flex-1 flex-col justify-center gap-3"
        role="status"
        aria-label="Проверяем вход…"
      >
        <Skeleton width="12rem" height="0.75rem" rounded="0.375rem" />
        <Skeleton width="20rem" height="2rem" rounded="0.5rem" />
        <Skeleton width="16rem" height="1rem" rounded="0.375rem" />
        <div class="mt-3 flex gap-3">
          <Skeleton width="12rem" height="3rem" rounded="0.75rem" />
          <Skeleton width="12rem" height="3rem" rounded="0.75rem" />
        </div>
      </div>
      <div
        v-else
        class="flex min-h-full w-full flex-1 flex-col"
        :class="route.name === 'home' ? '' : 'mx-auto max-w-2xl'"
      >
        <RouterView />
      </div>
    </main>
    <!-- Обновления: мягкий оффер поверх, жёсткий блок — вместо всего. -->
    <UpdateOffer v-if="updater.offerOpen && !updater.blocked" />
    <UpdateBlocked v-if="updater.blocked" />
  </div>
</template>

<style>
html,
body {
  background-color: var(--color-background-surface);
  color-scheme: dark;
}
</style>
