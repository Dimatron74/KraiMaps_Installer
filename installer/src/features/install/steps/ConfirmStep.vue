<script setup lang="ts">
import { computed } from 'vue';
import { useInstallStore } from '../../../app/stores/install';
import { LOADER_NAMES } from '../../../shared/types/installer';
import { isExactVersion } from '../../../shared/utils/version';
import Alert from '../../../shared/ui/Alert.vue';
import Btn from '../../../shared/ui/Btn.vue';

const store = useInstallStore();
const pack = computed(() => store.pack!);

/** Точная версия автора из манифеста — приоритет над хинтом архива (D7). */
const siteVersion = computed(() => {
  const v = store.siteMeta?.loaderVersion?.trim();
  return v && isExactVersion(v) ? v : null;
});

/** Человекочитаемый загрузчик: диапазон ("[40,)") никогда не показываем. */
const loaderDisplay = computed(() => {
  const name = LOADER_NAMES[pack.value.loader];
  if (siteVersion.value) return `${name} ${siteVersion.value}`;
  const hint = pack.value.loader_version_hint;
  if (hint && isExactVersion(hint)) return `${name} ${hint}`;
  return `${name} · не указана`;
});

function formatSize(bytes: number): string {
  const mb = bytes / 1024 / 1024;
  return mb >= 1 ? `${mb.toFixed(1)} МБ` : `${Math.round(bytes / 1024)} КБ`;
}
</script>

<template>
  <div v-if="pack">
    <h2 class="text-lg font-bold text-primary">Подтвердите установку</h2>
    <p class="mt-1 text-sm text-secondary">
      Без вашего согласия установщик ничего не меняет в игре.
    </p>

    <div class="glass-card mt-4 p-4">
      <div class="flex items-center gap-3">
        <!-- Иконка карты с сайта/архива; нет — стандартный блок-заглушка. -->
        <img
          v-if="store.siteIconUrl"
          :src="store.siteIconUrl"
          :alt="pack.world_name"
          class="h-10 w-10 shrink-0 rounded-lg border border-white/10 object-cover"
        />
        <div
          v-else
          class="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg bg-primary-500/10 border border-primary-500/20"
        >
          <svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="h-5 w-5 text-primary-400"><circle cx="15" cy="19" r="2"/><path d="M20.9 19.8A2 2 0 0 0 22 18V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2h5.1"/><path d="M15 11v-1"/><path d="M15 17v-2"/></svg>
        </div>
        <div class="min-w-0 flex-1">
          <p class="truncate text-sm font-semibold text-primary">{{ pack.world_name }}</p>
          <p class="mt-0.5 text-xs text-tertiary">{{ pack.file_name }} · {{ formatSize(pack.file_size_bytes) }}</p>
        </div>
        <span
          :class="[
            'shrink-0 rounded-full px-2.5 py-1 text-xs font-bold',
            pack.is_modded ? 'bg-warning-500/15 text-warning-400' : 'bg-success-500/15 text-success-400',
          ]"
        >
          {{ pack.is_modded ? 'С модами' : 'Ванилла' }}
        </span>
      </div>

      <dl class="mt-3 space-y-1.5 text-xs">
        <div class="flex justify-between gap-4">
          <dt class="text-tertiary">Версия мира</dt>
          <dd class="font-semibold text-primary">{{ pack.world_version ?? 'неизвестна' }}</dd>
        </div>
        <div v-if="pack.is_modded" class="flex justify-between gap-4">
          <dt class="text-tertiary">Загрузчик</dt>
          <dd class="font-semibold text-primary">{{ loaderDisplay }}</dd>
        </div>
        <div v-if="pack.is_modded" class="flex justify-between gap-4">
          <dt class="text-tertiary">Модов в архиве</dt>
          <dd class="font-semibold text-primary">{{ pack.mods.length }}</dd>
        </div>
      </dl>

      <Alert v-if="store.siteLoaderConflict" tone="warning" class="mt-3">
        <p class="text-xs">{{ store.siteLoaderConflict }}</p>
      </Alert>
    </div>

    <div v-if="pack.resource_packs.length > 0" class="glass-card mt-4 p-4">
      <p class="text-sm font-semibold text-primary">
        Ресурспаки
        <span class="font-normal text-tertiary">· поставятся в resourcepacks</span>
      </p>
      <ul class="mt-3 space-y-2">
        <li v-for="rp in pack.resource_packs" :key="rp.source">
          <label class="flex cursor-pointer items-center gap-3 rounded-xl border border-white/10 bg-white/[0.02] p-2.5 transition-colors hover:border-white/20">
            <input
              type="checkbox"
              :checked="store.enabledResourcePacks.includes(rp.source)"
              @change="store.toggleResourcePack(rp.source)"
              class="h-4 w-4 shrink-0 accent-emerald-500"
            />
            <img
              v-if="store.resourcePackIcons[rp.source]"
              :src="store.resourcePackIcons[rp.source]"
              :alt="rp.name"
              class="h-10 w-10 shrink-0 rounded-lg border border-white/10 object-cover"
            />
            <div
              v-else
              class="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg bg-primary-500/10 border border-primary-500/20"
            >
              <svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="h-5 w-5 text-primary-400"><rect width="18" height="18" x="3" y="3" rx="2"/><circle cx="9" cy="9" r="2"/><path d="m21 15-3.086-3.086a2 2 0 0 0-2.828 0L6 21"/></svg>
            </div>
            <div class="min-w-0 flex-1">
              <p class="truncate text-sm font-semibold text-primary">{{ rp.name }}</p>
              <p v-if="rp.description" class="truncate text-xs text-tertiary">{{ rp.description }}</p>
            </div>
            <span class="shrink-0 rounded-full bg-white/5 px-2 py-0.5 text-[11px] font-semibold text-tertiary">
              {{ rp.kind === 'zip' ? 'ZIP' : 'Папка' }}
            </span>
          </label>
        </li>
      </ul>
      <p class="mt-2 text-xs text-tertiary">Снимите галочку — этот пак ставиться не будет.</p>
    </div>

    <div class="mt-5 flex gap-2">
      <Btn variant="secondary" :disabled="store.busy" @click="store.reset()">Отмена</Btn>
      <Btn variant="success" full-width :loading="store.busy" @click="store.confirmPack()">
        {{ store.statusLine || 'Установить' }}
      </Btn>
    </div>
  </div>
</template>
