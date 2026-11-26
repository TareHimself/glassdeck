import { styled } from "@linaria/react";
import { describeError, useGlassdeckd } from "../glassdeckd";
import { Card, CardTitle, Title } from "./Page";

const Grid = styled.div`
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
  gap: 14px;
`;

const Value = styled.div`
  font-family: var(--mono);
  font-size: 13px;
  overflow-wrap: anywhere;
`;

export function Overview() {
  const connection = useGlassdeckd((s) => s.connection);
  return (
    <>
      <Title>Overview</Title>
      <Grid>
        <Card>
          <CardTitle>glassdeckd</CardTitle>
          <Value>{connection.state === "lost" ? describeError(connection.error) : connection.state}</Value>
        </Card>
        {connection.state === "connected" && (
          <Card>
            <CardTitle>Stream</CardTitle>
            <Value>
              {connection.status.stream.state}
              {connection.status.stream.playing ? `: ${connection.status.stream.playing}` : ""}
            </Value>
          </Card>
        )}
      </Grid>
    </>
  );
}
