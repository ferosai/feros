import type { Metadata } from "next";
import { Inter } from "next/font/google";
import { ThemeProvider } from "next-themes";
import { NuqsAdapter } from "nuqs/adapters/next/app";
import { Toaster } from "@/components/ui/sonner";
import { APP_SITE_DESCRIPTION, APP_TITLE } from "@/lib/constants";
import "./globals.css";

const inter = Inter({ subsets: ["latin"], preload: false });

export const metadata: Metadata = {
  title: APP_TITLE,
  description: APP_SITE_DESCRIPTION,
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="en" suppressHydrationWarning>
      <head>
        <script
          dangerouslySetInnerHTML={{
            __html: `var __name = function (target, value) { return Object.defineProperty(target, "name", { value, configurable: true }); };`,
          }}
        />
      </head>
      <body className={inter.className}>
        <NuqsAdapter>
          <ThemeProvider attribute="class" defaultTheme="light" enableSystem>
            {children}
            <Toaster position="bottom-right" />
          </ThemeProvider>
        </NuqsAdapter>
      </body>
    </html>
  );
}
