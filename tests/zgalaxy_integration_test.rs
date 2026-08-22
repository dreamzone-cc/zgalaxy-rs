//! Integration with the ZGALAXY engine: the planet artifact it serves must be
//! parseable by zgalaxy-rs. Set ZGALAXY_PLANET_FILE to a real planet file
//! (e.g. downloaded from <engine>/api/v1/planet/download) to run the live
//! check; otherwise the test is skipped.

#[test]
fn parse_zgalaxy_engine_planet() {
    let path = match std::env::var("ZGALAXY_PLANET_FILE") {
        Ok(p) if !p.is_empty() => p,
        _ => {
            eprintln!("skipping: ZGALAXY_PLANET_FILE not set");
            return;
        }
    };
    let data = std::fs::read(&path).expect("planet file readable");
    assert!(!data.is_empty(), "planet file is empty");
    match zgalaxy_rs::world::World::parse_binary(&data) {
        Ok(world) => {
            println!(
                "parsed planet: type={} roots={} signed={}",
                world.world_type,
                world.roots.len(),
                !world.signature.is_empty()
            );
            assert!(!world.roots.is_empty(), "planet has no roots");
        }
        Err(e) => panic!("zgalaxy-rs cannot parse the planet served by ZGALAXY: {e}"),
    }
}
