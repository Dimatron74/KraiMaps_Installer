/**
 * Приём zip-архива перетаскиванием на главную и на шаг выбора файла.
 *
 * Только нативные события Tauri: на Windows WebView2 перехватывает HTML5
 * drag-and-drop, и пути файлов в JS приходят лишь из `onDragDropEvent`.
 */
import { onMounted, onUnmounted, ref } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { useInstallStore } from '../../app/stores/install';
import { useNotificationsStore } from '../../app/stores/notifications';
import { useSiteInstallStore } from '../../app/stores/siteInstall';
import { useUpdaterStore } from '../../app/stores/updater';
import { isZipPath } from '../../shared/utils/zip';

export function useArchiveDrop() {
  const route = useRoute();
  const router = useRouter();
  const install = useInstallStore();
  const site = useSiteInstallStore();
  const updater = useUpdaterStore();
  const notifications = useNotificationsStore();

  const dragging = ref(false);
  let unlisten: (() => void) | null = null;
  let disposed = false;

  /** Дроп принимаем только там, где архив заменяет текущий выбор, а не сбивает флоу. */
  function canDrop(): boolean {
    if (updater.offerOpen || updater.blocked) return false;
    if (route.name === 'home') return true;
    return route.name === 'install' && site.step === 'idle' && install.step === 'file';
  }

  function accept(paths: string[]) {
    if (!canDrop()) return;
    // Пустой список — перетащили текст/ссылку, а не файл: подсказка не нужна.
    if (paths.length === 0) return;
    if (paths.length > 1) {
      notifications.warning('Только один архив', 'Перетащите один zip-архив карты за раз.');
      return;
    }
    const path = paths[0];
    if (path === undefined || !isZipPath(path)) {
      notifications.warning(
        'Нужен zip-архив',
        'Перетащите zip-архив карты — папки и другие файлы не подойдут.',
      );
      return;
    }

    if (route.name !== 'install') void router.push('/install');
    install.loadRoots();
    void install.pickZip(path);
  }

  onMounted(async () => {
    try {
      const { getCurrentWebview } = await import('@tauri-apps/api/webview');
      const stop = await getCurrentWebview().onDragDropEvent(({ payload }) => {
        switch (payload.type) {
          case 'enter':
            dragging.value = canDrop() && payload.paths.length > 0;
            break;
          case 'over':
            if (!canDrop()) dragging.value = false;
            break;
          case 'leave':
            dragging.value = false;
            break;
          case 'drop':
            dragging.value = false;
            accept(payload.paths);
            break;
        }
      });
      if (disposed) {
        stop();
      } else {
        unlisten = () => stop();
      }
    } catch {
    }
  });

  onUnmounted(() => {
    disposed = true;
    unlisten?.();
  });

  return { dragging };
}
