import { NextResponse } from "next/server";
import type { NextRequest } from "next/server";

export function middleware(req: NextRequest) {
  const apiKey = process.env.STUDIO_API_KEY;

  // No API key configured — middleware is a complete no-op.
  // Open-source users who don't set STUDIO_API_KEY get vanilla behavior.
  if (!apiKey) {
    return NextResponse.next();
  }

  // ── Basic Auth Gate ────────────────────────────────────────────
  let authenticated = false;
  const basicAuth = req.headers.get("authorization");
  
  if (basicAuth && basicAuth.startsWith("Basic ")) {
    const decoded = atob(basicAuth.split(" ")[1]);
    const colonIdx = decoded.indexOf(":");
    if (colonIdx !== -1 && decoded.substring(colonIdx + 1) === apiKey) {
      authenticated = true;
    }
  }

  if (!authenticated) {
    return new NextResponse("Auth required", {
      status: 401,
      headers: {
        "WWW-Authenticate": 'Basic realm="Feros Studio Secure Area"',
      },
    });
  }

  // ── Backend API Proxy ──────────────────────────────────────────
  // Rewrites /api-proxy/* → BACKEND_API_URL/* and injects X-API-Key.
  if (req.nextUrl.pathname.startsWith('/api-proxy/')) {
    const backendUrlString = process.env.BACKEND_API_URL || "https://feros-studio-api.fly.dev";
    
    const targetUrl = new URL(
      req.nextUrl.pathname.replace(/^\/api-proxy/, ''),
      backendUrlString
    );
    targetUrl.search = req.nextUrl.search;
    
    const requestHeaders = new Headers(req.headers);
    requestHeaders.set("X-API-Key", apiKey);
    
    return NextResponse.rewrite(targetUrl, {
      request: {
        headers: requestHeaders,
      },
    });
  }

  return NextResponse.next();
}

export const config = {
  matcher: [
    /*
     * Match all request paths except for the ones starting with:
     * - api/ (Local Next.js routes like OAuth)
     * - _next/static (static files)
     * - _next/image (image optimization files)
     * - favicon.ico, sitemap.xml, robots.txt (metadata files)
     */
    '/((?!api\/|_next/static|_next/image|favicon.ico|sitemap.xml|robots.txt).*)',
  ],
};
