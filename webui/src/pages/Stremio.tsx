import { Card } from '../components/Card'
import { StatusPill } from '../components/Status'
import styles from './Pages.module.css'

/**
 * Deliberate placeholder. The route, the navigation slot and the page shell
 * exist so the next milestone drops the real integration in here without
 * touching the shell or the data layer.
 */
export function Stremio() {
  return (
    <div className={styles.stack}>
      <Card title="Stremio" aside={<StatusPill tone="neutral">Devre dışı</StatusPill>}>
        <div className={styles.placeholder}>
          <span className={styles.placeholderTitle}>Bir sonraki milestone'da etkinleştirilecek</span>
          <p className={styles.note}>
            Stremio entegrasyonu bu sürümün kapsamı dışındadır. Bu bölüm yalnızca yerini ayırır;
            arka planda hiçbir Stremio servisi çalışmaz ve hiçbir istek gönderilmez.
          </p>
          <ul className={styles.list}>
            <li>Katalog gezinme</li>
            <li>Eklenti yönetimi</li>
            <li>Kodi'ye oynatma aktarımı</li>
          </ul>
        </div>
      </Card>
    </div>
  )
}
