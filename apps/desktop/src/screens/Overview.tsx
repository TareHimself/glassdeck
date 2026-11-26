import { Card } from "../components/Card";
import { PageHeader } from "../components/PageHeader";
import { describeError, useGlassdeckd } from "../glassdeckd";
import styles from "./Overview.module.css";

export function Overview() {
  const connection = useGlassdeckd((s) => s.connection);
  return (
    <div className={styles.page}>
      <PageHeader title="Overview" />
      <div className={styles.grid}>
        <Card title="glassdeckd">
          <div className={styles.value}>{connection.state === "lost" ? describeError(connection.error) : connection.state}</div>
        </Card>
        {connection.state === "connected" && (
          <Card title="Stream">
            <div className={styles.value}>
              {connection.status.stream.state}
              {connection.status.stream.playing ? `: ${connection.status.stream.playing}` : ""}
            </div>
          </Card>
        )}
      </div>
    </div>
  );
}
