<script setup lang="ts">
import { computed } from 'vue';
import { useInstallStore } from '../../../app/stores/install';
import { LAUNCHER_NAMES } from '../../../shared/types/installer';
import Alert from '../../../shared/ui/Alert.vue';
import Btn from '../../../shared/ui/Btn.vue';

const store = useInstallStore();

/**
 * Общая папка mods/ — только у TLauncher: у него одна папка игры на все
 * версии, поэтому моды действительно затрагивают другие версии.
 *
 * Legacy Launcher сюда НЕ относится: он раскладывает `home/<версия>/`, то есть
 * у каждой версии своя папка. Раньше сюда попадал и Legacy, и текст врал
 * пользователю («у LL нет инстансов, моды общие») — реальная причина была
 * другой: удаление версии в лаунчере не удаляет `home/<версия>`, и там остались
 * моды прошлой карты.
 */
const sharedFolder = computed(() => store.target?.launcher === 'tlauncher');

/**
 * Выбранная папка — ровно та, что нужна карте.
 *
 * Именно этот случай ломал экран: папка версии у Legacy и TLauncher одна на
 * комбинацию (версия игры, загрузчик, версия загрузчика), поэтому второй
 * такой инстанс создать нельзя, а кнопка «создать новый» обещала копию и
 * приводила в ту же папку.
 */
const sameVersionSlot = computed(
  () =>
    !!store.takenVersionTarget &&
    !!store.target &&
    store.takenVersionTarget.path === store.target.path,
);

/**
 * Кнопка «создать новый инстанс» нужна только когда ведёт в ДРУГУЮ папку.
 *
 * У Legacy и TLauncher это так, лишь если выбрана другая версия (например у
 * карты Fabric 0.17, а выбрана заваленная модами Fabric 0.15) — тогда новая
 * версия честный выход, и кнопка остаётся. У официального лаунчера и
 * лаунчеров с настоящими инстансами второй инстанс заводится всегда.
 */
const canCreateInstance = computed(() => {
  const kind = store.target?.launcher;
  if (!kind || kind === 'manual') return false;
  if (kind !== 'legacy' && kind !== 'tlauncher') return true;
  return !sameVersionSlot.value;
});

const launcherName = computed(() =>
  store.target ? LAUNCHER_NAMES[store.target.launcher] : 'лаунчер',
);

/** Заголовок и первая строка — по ситуации, без вложенных v-if в разметке. */
const title = computed(() => {
  if (sharedFolder.value) return 'Внимание: общие моды';
  if (store.target?.launcher === 'official') return 'В этой установке уже есть моды';
  return 'В этой версии уже есть моды';
});

const lead = computed(() => {
  if (sharedFolder.value) {
    return `Модовая карта заменит общие моды — у ${launcherName.value} одна папка игры на все версии.`;
  }
  if (store.target) return 'В папке этой версии уже лежат моды — модовая карта заменит их.';
  return '';
});

/** Почему кнопки «создать новый» нет: без объяснения её отсутствие выглядит
 * как недоработка. */
const noSecondInstance = computed(() => !canCreateInstance.value && sameVersionSlot.value);
</script>

<template>
  <div>
    <h2 class="text-lg font-bold text-primary">{{ title }}</h2>
    <Alert tone="warning" class="mt-3">
      <p class="font-semibold">{{ lead }}</p>
      <p class="mt-1 text-xs">
        Папка <span class="font-mono">mods/</span> будет очищена и заполнена модами карты, остальные
        файлы карты (настройки, скрипты, ресурсы автора) — поставлены поверх. Всё удалённое и
        заменённое сохранится в <span class="font-mono">.kraimaps-backup/&lt;дата&gt;/</span> —
        можно откатить вручную. Мир и прогресс в <span class="font-mono">saves/</span> не тронем.
      </p>
      <p v-if="noSecondInstance" class="mt-2 text-xs">
        Второй такой инстанс {{ launcherName }} не заведёт: папка версии одна на версию игры и
        загрузчик, поэтому такой же сделать нельзя. Старые моды не пропадут — они останутся в
        бэкапе, верните их вручную, если понадобятся.
      </p>
      <p v-if="store.target?.path" class="mt-2 break-all text-[11px] text-tertiary">
        Папка: <span class="font-mono">{{ store.target.path }}</span>
      </p>
    </Alert>

    <div class="mt-5 flex flex-col gap-2">
      <Btn
        v-if="canCreateInstance"
        variant="success"
        :loading="store.busy"
        @click="store.createNewTarget()"
      >
        Создать новый инстанс (рекомендуется)
      </Btn>
      <div class="flex gap-2">
        <Btn variant="secondary" @click="store.step = 'target'">Назад</Btn>
        <Btn variant="danger" :loading="store.busy" @click="store.confirmWipe()">
          Всё равно ставить сюда
        </Btn>
      </div>
    </div>
  </div>
</template>
