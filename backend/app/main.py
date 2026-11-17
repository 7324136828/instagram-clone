"""FastAPI entry point of the Instagram clone backend."""

from __future__ import annotations

import os
from collections.abc import AsyncIterator, Iterable, Sequence
from contextlib import asynccontextmanager
from pathlib import Path

from fastapi import Depends, FastAPI, Request, Response
from fastapi.concurrency import run_in_threadpool
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import FileResponse, JSONResponse
from pydantic import BaseModel, ConfigDict

from backend.settings import configured_allowed_hosts
from backend.settings import configured_cache_directory as resolve_cache_directory
from backend.settings import configured_database_path as resolve_database_path

from .guard import RequestGuard, is_local_client
from .library import (
    MAX_POST_EDIT_BYTES,
    JsonObject,
    Library,
    LibraryError,
    LibraryLimits,
    parse_post_changes,
    read_body,
)
from .store import CounterState, CounterStore

PROJECT_ROOT = Path(__file__).resolve().parents[2]


class CounterResponse(BaseModel):
    model_config = ConfigDict(from_attributes=True)

    count: int
    updated_at: str


def configured_database_path() -> Path:
    return resolve_database_path(PROJECT_ROOT)


def configured_cache_directory() -> Path:
    return resolve_cache_directory(PROJECT_ROOT)


DEV_CORS_ORIGINS = "http://localhost:5173,http://127.0.0.1:5173"


def configured_cors_origins(default: str = DEV_CORS_ORIGINS) -> list[str]:
    """CORS_ORIGINS, else `default` (the Vite dev server; the packaged server passes "")."""
    value = os.environ.get("CORS_ORIGINS", default)
    return [origin.strip() for origin in value.split(",") if origin.strip()]


def for_client(request: Request, library: JsonObject) -> JsonObject:
    """A library answer for this client: another computer is not told the folder's full path
    (it names the Windows user); it sees the folder's name only."""
    if is_local_client(request.scope):
        return library
    folder = Path(str(library.get("directory", ""))).name or "the library folder"
    return {**library, "directory": f"{folder} on the computer running the backend"}


def get_store(request: Request) -> CounterStore:
    store: CounterStore = request.app.state.counter_store
    return store


def get_library(request: Request) -> Library:
    library: Library = request.app.state.library
    return library


def _content_length(request: Request) -> int | None:
    value = request.headers.get("content-length", "")
    return int(value) if value.isdigit() else None


def create_app(
    database_path: Path | None = None,
    cache_directory: Path | None = None,
    *,
    library_limits: LibraryLimits | None = None,
    cors_origins: Sequence[str] | None = None,
    allowed_hosts: Iterable[str] | None = None,
) -> FastAPI:
    """The API. The library folder (cache_directory, else configured_cache_directory()) is
    resolved on first use, so an unusable folder only turns the /api/library routes into 503s.
    cors_origins defaults to configured_cors_origins(), allowed_hosts (extra host names) to
    configured_allowed_hosts(), read on first need."""
    @asynccontextmanager
    async def lifespan(application: FastAPI) -> AsyncIterator[None]:
        store = CounterStore(
            database_path if database_path is not None else configured_database_path()
        )
        store.initialize()
        application.state.counter_store = store
        try:
            yield
        finally:
            # Unlocks the library folder so the next run (or a reload) can use it.
            await run_in_threadpool(application.state.library.close)

    application = FastAPI(
        title="Instagram Clone API",
        version="1.0.0",
        lifespan=lifespan,
    )
    application.state.library = Library(
        (lambda: cache_directory) if cache_directory is not None else configured_cache_directory,
        library_limits,
    )
    origins = list(cors_origins) if cors_origins is not None else configured_cors_origins()
    application.add_middleware(
        CORSMiddleware,
        allow_origins=origins,
        allow_methods=["GET", "POST", "PUT", "PATCH", "DELETE"],
        allow_headers=["*"],
    )
    # Added last, so it runs first: a refused request never reaches CORS or a route.
    application.add_middleware(
        RequestGuard,
        trusted_origins=origins,
        allowed_hosts=list(allowed_hosts) if allowed_hosts is not None else configured_allowed_hosts,
    )

    @application.exception_handler(LibraryError)
    async def library_error(_: Request, error: LibraryError) -> JSONResponse:
        return JSONResponse({"detail": error.detail}, status_code=error.status_code)

    @application.get("/api/health")
    def health() -> dict[str, str]:
        return {"status": "ok"}

    @application.get("/api/counter", response_model=CounterResponse)
    def read_counter(
        counter_store: CounterStore = Depends(get_store),
    ) -> CounterState:
        return counter_store.get()

    @application.post("/api/counter/click", response_model=CounterResponse)
    def record_click(
        counter_store: CounterStore = Depends(get_store),
    ) -> CounterState:
        return counter_store.increment()

    @application.delete("/api/counter", response_model=CounterResponse)
    def clear_counter(
        counter_store: CounterStore = Depends(get_store),
    ) -> CounterState:
        return counter_store.reset()

    @application.get("/api/library")
    def read_library(request: Request, library: Library = Depends(get_library)) -> JSONResponse:
        return JSONResponse(for_client(request, library.snapshot()))

    @application.delete("/api/library", status_code=204)
    def clear_library(library: Library = Depends(get_library)) -> Response:
        library.clear()
        return Response(status_code=204)

    @application.api_route("/api/library/files/{file_path:path}", methods=["GET", "HEAD"])
    def read_library_file(
        file_path: str, library: Library = Depends(get_library)
    ) -> FileResponse:
        path, media, info = library.media_file(file_path)
        return FileResponse(
            path,
            media_type=media.mime,
            stat_result=info,
            headers={"X-Content-Type-Options": "nosniff", "Cache-Control": "no-cache"},
        )

    @application.post("/api/library/uploads", status_code=201)
    def create_library_upload(library: Library = Depends(get_library)) -> JSONResponse:
        return JSONResponse({"upload_id": library.create_upload()}, status_code=201)

    @application.put("/api/library/uploads/{upload_id}/files/{name}")
    async def upload_library_file(
        upload_id: str, name: str, request: Request, library: Library = Depends(get_library)
    ) -> JSONResponse:
        stored = await library.receive_file(
            upload_id, name, request.stream(), _content_length(request)
        )
        return JSONResponse(stored)

    @application.post("/api/library/uploads/{upload_id}/commit")
    async def commit_library_upload(
        upload_id: str, request: Request, library: Library = Depends(get_library)
    ) -> JSONResponse:
        body = await read_body(request.stream(), library.limits.max_commit_bytes)
        return JSONResponse(for_client(request, await run_in_threadpool(library.commit, upload_id, body)))

    @application.delete("/api/library/uploads/{upload_id}", status_code=204)
    def abort_library_upload(upload_id: str, library: Library = Depends(get_library)) -> Response:
        library.abort_upload(upload_id)
        return Response(status_code=204)

    @application.delete("/api/library/posts/{post_id}")
    def delete_library_post(
        post_id: str, request: Request, library: Library = Depends(get_library)
    ) -> JSONResponse:
        return JSONResponse(for_client(request, library.delete_post(post_id)))

    @application.patch("/api/library/posts/{post_id}")
    async def update_library_post(
        post_id: str, request: Request, library: Library = Depends(get_library)
    ) -> JSONResponse:
        """Edits a post: {"caption": "..."} replaces its description."""
        changes = parse_post_changes(await read_body(request.stream(), MAX_POST_EDIT_BYTES))
        updated = await run_in_threadpool(library.update_post, post_id, changes)
        return JSONResponse(for_client(request, updated))

    return application


app = create_app()
