import { NextRequest, NextResponse } from "next/server";

// BACKEND_API_URL is the docker-internal bridge URL (http://studio-api:8000).
// NEXT_PUBLIC_API_URL is the browser-facing URL (http://localhost:8000).
const BACKEND = process.env.BACKEND_API_URL || process.env.NEXT_PUBLIC_API_URL || "http://localhost:8000";

/**
 * Proxy the OAuth callback from Google (or any provider) to the backend.
 *
 * Flow:
 *   Google → http://localhost:3000/api/oauth/callback?code=xxx&state=xxx
 *   → this route forwards to http://localhost:8000/api/oauth/callback?code=xxx&state=xxx
 *   → backend stores credential, returns HTML with postMessage + window.close()
 *   → we pass that HTML back to the popup
 */
export async function GET(request: NextRequest) {
  const params = request.nextUrl.searchParams.toString();
  const backendUrl = `${BACKEND}/api/oauth/callback?${params}`;

  try {
    const resp = await fetch(backendUrl);
    const html = await resp.text();
    return new NextResponse(html, {
      status: resp.status,
      headers: { "Content-Type": "text/html; charset=utf-8" },
    });
  } catch (err) {
    return new NextResponse(
      `<html><body><h2>OAuth relay error</h2><p>${err}</p>
      <script>setTimeout(()=>window.close(),3000)</script></body></html>`,
      { status: 502, headers: { "Content-Type": "text/html" } }
    );
  }
}
