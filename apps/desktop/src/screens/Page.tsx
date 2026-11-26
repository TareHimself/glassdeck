import { styled } from "@linaria/react";

export const Title = styled.h1`
  margin: 0 0 20px;
  font-size: 32px;
  line-height: 1.1;
  letter-spacing: -0.02em;
  font-weight: 700;
`;

export const Card = styled.section`
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: var(--radius);
  padding: 16px;
`;

export const CardTitle = styled.h2`
  margin: 0 0 10px;
  font-size: 13px;
  font-weight: 600;
  color: var(--muted);
`;
