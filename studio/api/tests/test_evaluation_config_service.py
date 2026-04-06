from __future__ import annotations

import uuid

import pytest

from app.models.evaluation import EvaluationConfig, EvaluationConfigVersion
from app.schemas.evaluation import EvaluationConfigPayload
from app.services.evaluations.config_service import EvaluationConfigService


class _FakeSession:
    def __init__(self) -> None:
        self.added: list[object] = []

    def add(self, obj: object) -> None:
        self.added.append(obj)

    async def flush(self) -> None:
        for obj in self.added:
            if hasattr(obj, "id") and obj.id is None:
                obj.id = uuid.uuid4()


@pytest.mark.asyncio
async def test_create_config_creates_initial_version() -> None:
    db = _FakeSession()
    service = EvaluationConfigService()
    config, version = await service.create_config(
        db,
        agent_id=uuid.uuid4(),
        name="Smoke config",
        payload=EvaluationConfigPayload(),
    )
    assert isinstance(config, EvaluationConfig)
    assert isinstance(version, EvaluationConfigVersion)
    assert config.latest_version == 1
    assert version.version == 1
    assert version.config_json["scenario_profile"] == "balanced"


@pytest.mark.asyncio
async def test_create_version_increments_latest(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    db = _FakeSession()
    service = EvaluationConfigService()
    cfg = EvaluationConfig(
        id=uuid.uuid4(),
        agent_id=uuid.uuid4(),
        name="v",
        latest_version=2,
    )

    async def _fake_get_config(*args: object, **kwargs: object) -> EvaluationConfig:
        return cfg

    monkeypatch.setattr(
        service,
        "get_config",
        _fake_get_config,
    )
    version = await service.create_version(
        db,
        agent_id=cfg.agent_id,
        config_id=cfg.id,
        payload=EvaluationConfigPayload(),
    )
    assert version is not None
    assert version.version == 3
    assert cfg.latest_version == 3
