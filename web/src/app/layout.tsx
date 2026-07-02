import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "Silences — Agentic Coding",
  description: "Silences agentic coding framework",
};

export default function RootLayout({
  children,
}: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="zh-CN" className="h-full">
      <body className="h-full">{children}</body>
    </html>
  );
}
