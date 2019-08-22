// Copyright 2018-2019 Chainpool.

use super::*;

use serde_json::{json, Value};

use std::iter::FromIterator;

use parity_codec::{Decode, Encode};
use rustc_hex::FromHex;
// substrate
use primitives::crypto::UncheckedInto;
use primitives::{Blake2Hasher, H160, H256};
use runtime_primitives::generic::SignedBlock;
use runtime_primitives::traits::{As, Block as BlockT, NumberFor, ProvideRuntimeApi};

// chainx
use chainx_primitives::AccountIdForRpc;
use chainx_runtime::Call;
use xr_primitives::AddrStr;

use xassets::{AssetLimit, AssetType, Chain, ChainT};
use xbridge_common::types::GenericAllSessionInfo;
use xbridge_features::{
    self,
    crosschain_binding::{BitcoinAddress, EthereumAddress},
};
use xspot::{HandicapInfo, OrderIndex, OrderInfo, TradingPair, TradingPairIndex};
use xtokens::DepositVoteWeight;

use crate::chainx::chainx_trait::ChainXApi;
use crate::chainx::utils::*;

/// Convert &[u8] to String
macro_rules! String {
    ($str:expr) => {
        String::from_utf8_lossy($str).into_owned()
    };
}

impl<B, E, Block, RA>
    ChainXApi<
        NumberFor<Block>,
        <Block as BlockT>::Hash,
        AccountIdForRpc,
        Balance,
        BlockNumber,
        SignedBlock<Block>,
    > for ChainX<B, E, Block, RA>
where
    B: client::backend::Backend<Block, Blake2Hasher> + Send + Sync + 'static,
    E: client::CallExecutor<Block, Blake2Hasher> + Clone + Send + Sync + 'static,
    Block: BlockT<Hash = H256> + 'static,
    RA: Send + Sync + 'static,
    client::Client<B, E, Block, RA>: ProvideRuntimeApi,
    <client::Client<B, E, Block, RA> as ProvideRuntimeApi>::Api: Metadata<Block>
        + XAssetsApi<Block>
        + XMiningApi<Block>
        + XSpotApi<Block>
        + XFeeApi<Block>
        + XStakingApi<Block>
        + XBridgeApi<Block>,
{
    fn block_info(&self, number: Option<NumberFor<Block>>) -> Result<Option<SignedBlock<Block>>> {
        Ok(self.client.block(&self.block_id_by_number(number)?)?)
    }

    fn next_renominate(
        &self,
        who: AccountIdForRpc,
        hash: Option<<Block as BlockT>::Hash>,
    ) -> Result<Option<BlockNumber>> {
        let state = self.state_at(hash)?;
        let who: AccountId = who.unchecked_into();
        let key = <xstaking::LastRenominationOf<Runtime>>::key_for(&who);
        if let Some(last_renomination) =
            Self::pickout::<BlockNumber>(&state, &key, Hasher::BLAKE2256)?
        {
            let key = <xstaking::BondingDuration<Runtime>>::key();
            if let Some(bonding_duration) =
                Self::pickout::<BlockNumber>(&state, &key, Hasher::TWOX128)?
            {
                return Ok(Some(last_renomination + bonding_duration));
            }
        }
        Ok(None)
    }

    fn assets_of(
        &self,
        who: AccountIdForRpc,
        page_index: u32,
        page_size: u32,
        hash: Option<<Block as BlockT>::Hash>,
    ) -> Result<Option<PageData<AssetInfo>>> {
        let assets = self.valid_assets_of(self.block_id_by_hash(hash)?, who.unchecked_into())?;
        let final_result = assets
            .into_iter()
            .map(|(token, map)| {
                let mut bmap = BTreeMap::<AssetType, Balance>::from_iter(
                    xassets::AssetType::iterator().map(|t| (*t, Zero::zero())),
                );
                bmap.extend(map.iter());
                AssetInfo {
                    name: String!(&token),
                    details: bmap,
                }
            })
            .collect();
        into_pagedata(final_result, page_index, page_size)
    }

    fn assets(
        &self,
        page_index: u32,
        page_size: u32,
        hash: Option<<Block as BlockT>::Hash>,
    ) -> Result<Option<PageData<TotalAssetInfo>>> {
        let assets = self.all_assets(self.block_id_by_hash(hash)?)?;

        let state = self.state_at(hash)?;

        let mut all_assets = Vec::new();

        for (asset, valid) in assets.into_iter() {
            let mut bmap = BTreeMap::<AssetType, Balance>::from_iter(
                xassets::AssetType::iterator().map(|t| (*t, Zero::zero())),
            );

            let key = <xassets::TotalAssetBalance<Runtime>>::key_for(asset.token().as_ref());
            if let Some(info) =
                Self::pickout::<BTreeMap<AssetType, Balance>>(&state, &key, Hasher::BLAKE2256)?
            {
                bmap.extend(info.iter());
            }

            let mut lmap = BTreeMap::<AssetLimit, bool>::from_iter(
                xassets::AssetLimit::iterator().map(|t| (*t, true)),
            );
            let key = <xassets::AssetLimitProps<Runtime>>::key_for(asset.token().as_ref());
            if let Some(limit) =
                Self::pickout::<BTreeMap<AssetLimit, bool>>(&state, &key, Hasher::BLAKE2256)?
            {
                lmap.extend(limit.iter());
            }

            all_assets.push(TotalAssetInfo::new(asset, valid, bmap, lmap));
        }

        into_pagedata(all_assets, page_index, page_size)
    }

    fn verify_addr(
        &self,
        token: String,
        addr: String,
        memo: String,
        hash: Option<<Block as BlockT>::Hash>,
    ) -> Result<Option<bool>> {
        let token: xassets::Token = token.as_bytes().to_vec();
        let addr: AddrStr = addr.as_bytes().to_vec();
        let memo: xassets::Memo = memo.as_bytes().to_vec();

        if let Err(_e) = xassets::is_valid_token(&token) {
            return Ok(Some(false));
        }

        if addr.len() > 256 || memo.len() > 256 {
            return Ok(Some(false));
        }

        let ret = self
            .client
            .runtime_api()
            .verify_address(&self.block_id_by_hash(hash)?, token, addr, memo)
            .and_then(|r| match r {
                Ok(()) => Ok(None),
                Err(s) => Ok(Some(String!(s.as_ref()))),
            });
        let is_valid = match ret {
            Err(_) | Ok(Some(_)) => false,
            Ok(None) => true,
        };
        Ok(Some(is_valid))
    }

    fn withdrawal_limit(
        &self,
        token: String,
        hash: Option<<Block as BlockT>::Hash>,
    ) -> Result<Option<WithdrawalLimit<Balance>>> {
        let token: xassets::Token = token.as_bytes().to_vec();

        if xassets::is_valid_token(&token).is_err() {
            return Ok(None);
        }
        self.withdrawal_limit(self.block_id_by_hash(hash)?, token)
    }

    fn deposit_limit(
        &self,
        token: String,
        hash: Option<<Block as BlockT>::Hash>,
    ) -> Result<Option<DepositLimit>> {
        let token: xassets::Token = token.as_bytes().to_vec();

        if xassets::is_valid_token(&token).is_err() {
            return Ok(None);
        }
        let state = self.state_at(hash)?;
        // todo use `cando` to refactor if
        if token.as_slice() == xbitcoin::Module::<Runtime>::TOKEN {
            let key = <xbitcoin::BtcMinDeposit<Runtime>>::key();
            Self::pickout::<u64>(&state, &key, Hasher::TWOX128).map(|value| {
                Some(DepositLimit {
                    minimal_deposit: value.unwrap_or(As::sa(100000)),
                })
            })
        } else {
            return Ok(None);
        }
    }

    fn deposit_list(
        &self,
        chain: Chain,
        page_index: u32,
        page_size: u32,
        hash: Option<<Block as BlockT>::Hash>,
    ) -> Result<Option<PageData<DepositInfo>>> {
        let list = self
            .deposit_list_of(self.block_id_by_hash(hash)?, chain)
            .unwrap_or_default();

        // convert recordinfo to deposit
        let records: Vec<DepositInfo> = list.into_iter().map(Into::into).collect();
        into_pagedata(records, page_index, page_size)
    }

    fn withdrawal_list(
        &self,
        chain: Chain,
        page_index: u32,
        page_size: u32,
        hash: Option<<Block as BlockT>::Hash>,
    ) -> Result<Option<PageData<WithdrawInfo>>> {
        let list = self
            .withdrawal_list_of(self.block_id_by_hash(hash)?, chain)
            .unwrap_or_default();
        let records: Vec<WithdrawInfo> = list.into_iter().map(Into::into).collect();
        into_pagedata(records, page_index, page_size)
    }

    fn nomination_records(
        &self,
        who: AccountIdForRpc,
        hash: Option<<Block as BlockT>::Hash>,
    ) -> Result<Option<Vec<(AccountIdForRpc, NominationRecordForRpc)>>> {
        let state = self.state_at(hash)?;

        let mut records = Vec::new();

        let intentions = self.intention_set(self.block_id_by_hash(hash)?)?;
        let who: AccountId = who.unchecked_into();

        for intention in intentions {
            let nr_key = (who.clone(), intention.clone());
            self.nomination_record_v1_does_not_exist(&state, &nr_key)?;
            let record = self.get_nomination_record(&state, &nr_key)?;
            records.push((intention.into(), record.into()));
        }

        Ok(Some(records))
    }

    fn nomination_records_v1(
        &self,
        who: AccountIdForRpc,
        hash: Option<<Block as BlockT>::Hash>,
    ) -> Result<Option<Vec<(AccountIdForRpc, NominationRecordV1ForRpc)>>> {
        let state = self.state_at(hash)?;

        let mut records = Vec::new();

        let intentions = self.intention_set(self.block_id_by_hash(hash)?)?;
        let who: AccountId = who.unchecked_into();

        for intention in intentions {
            let nr_key = (who.clone(), intention.clone());
            if let Some(record) = self.into_or_get_nomination_record_v1(&state, &nr_key)? {
                records.push((intention.into(), record.into()));
            }
        }

        Ok(Some(records))
    }

    fn intention(
        &self,
        who: AccountIdForRpc,
        hash: Option<<Block as BlockT>::Hash>,
    ) -> Result<Option<Value>> {
        let state = self.state_at(hash)?;
        let who: AccountId = who.unchecked_into();

        let session_key: AccountIdForRpc =
            if let Ok(session_key) = self.get_session_key(&state, &who) {
                session_key.unwrap_or(who.clone()).into()
            } else {
                return Ok(None);
            };

        let jackpot_account =
            self.jackpot_accountid_for_unsafe(self.block_id_by_hash(hash)?, who)?;
        Ok(Some(json!({
            "sessionKey": session_key,
            "jackpotAccount": jackpot_account,
        })))
    }

    fn intentions(
        &self,
        hash: Option<<Block as BlockT>::Hash>,
    ) -> Result<Option<Vec<IntentionInfo>>> {
        let state = self.state_at(hash)?;
        let block_id = self.block_id_by_hash(hash)?;

        let mut intentions_info = Vec::new();
        for info_wrapper in self.get_intentions_info_wrapper(&state, block_id)? {
            if info_wrapper.intention_profs_wrapper.is_err() {
                return Err(ErrorKind::DeprecatedV0Err("chainx_getIntentions".into()).into());
            }
            intentions_info.push(info_wrapper.into());
        }

        Ok(Some(intentions_info))
    }

    fn intentions_v1(
        &self,
        hash: Option<<Block as BlockT>::Hash>,
    ) -> Result<Option<Vec<IntentionInfoV1>>> {
        let state = self.state_at(hash)?;
        let block_id = self.block_id_by_hash(hash)?;

        Ok(Some(
            self.get_intentions_info_wrapper(&state, block_id)?
                .into_iter()
                .map(Into::into)
                .collect(),
        ))
    }

    fn psedu_intentions(
        &self,
        hash: Option<<Block as BlockT>::Hash>,
    ) -> Result<Option<Vec<PseduIntentionInfo>>> {
        let state = self.state_at(hash)?;
        let block_id = self.block_id_by_hash(hash)?;

        let mut psedu_intentions_info = Vec::new();
        for info_wrapper in self.get_psedu_intentions_info_wrapper(&state, block_id)? {
            if info_wrapper.psedu_intention_profs_wrapper.is_err() {
                return Err(ErrorKind::DeprecatedV0Err("chainx_getPseduIntentions".into()).into());
            }
            psedu_intentions_info.push(info_wrapper.into());
        }

        Ok(Some(psedu_intentions_info))
    }

    fn psedu_intentions_v1(
        &self,
        hash: Option<<Block as BlockT>::Hash>,
    ) -> Result<Option<Vec<PseduIntentionInfoV1>>> {
        let state = self.state_at(hash)?;
        let block_id = self.block_id_by_hash(hash)?;

        Ok(Some(
            self.get_psedu_intentions_info_wrapper(&state, block_id)?
                .into_iter()
                .map(Into::into)
                .collect(),
        ))
    }

    fn psedu_nomination_records(
        &self,
        who: AccountIdForRpc,
        hash: Option<<Block as BlockT>::Hash>,
    ) -> Result<Option<Vec<PseduNominationRecord>>> {
        let state = self.state_at(hash)?;

        let who: AccountId = who.unchecked_into();

        let mut psedu_records = Vec::new();

        for token in self.get_psedu_intentions(&state)? {
            let wt_key = (who.clone(), token.clone());

            self.deposit_record_v1_does_not_exist(&state, &wt_key)?;

            let mut record = PseduNominationRecord::default();

            let key = <xtokens::DepositRecords<Runtime>>::key_for(&wt_key);
            if let Some(vote_weight) =
                Self::pickout::<DepositVoteWeight<BlockNumber>>(&state, &key, Hasher::BLAKE2256)?
            {
                record.last_total_deposit_weight = vote_weight.last_deposit_weight;
                record.last_total_deposit_weight_update = vote_weight.last_deposit_weight_update;
            }

            record.common = self.get_psedu_nomination_record_common(&state, &who, &token)?;

            psedu_records.push(record);
        }

        Ok(Some(psedu_records))
    }

    fn psedu_nomination_records_v1(
        &self,
        who: AccountIdForRpc,
        hash: Option<<Block as BlockT>::Hash>,
    ) -> Result<Option<Vec<PseduNominationRecordV1>>> {
        let state = self.state_at(hash)?;
        let mut psedu_records = Vec::new();

        let who: AccountId = who.unchecked_into();

        for token in self.get_psedu_intentions(&state)? {
            let mut record = PseduNominationRecordV1::default();
            record.common = self.get_psedu_nomination_record_common(&state, &who, &token)?;

            if let Some(vote_weight) =
                self.into_or_get_deposit_vote_weight_v1(&state, &(who.clone(), token))?
            {
                record.last_total_deposit_weight = format!("{}", vote_weight.last_deposit_weight);
                record.last_total_deposit_weight_update = vote_weight.last_deposit_weight_update;
            }

            psedu_records.push(record);
        }

        Ok(Some(psedu_records))
    }

    fn trading_pairs(
        &self,
        hash: Option<<Block as BlockT>::Hash>,
    ) -> Result<Option<Vec<(PairInfo)>>> {
        let mut pairs = Vec::new();
        let state = self.state_at(hash)?;

        let len_key = <xspot::TradingPairCount<Runtime>>::key();
        if let Some(len) = Self::pickout::<TradingPairIndex>(&state, &len_key, Hasher::TWOX128)? {
            for i in 0..len {
                let key = <xspot::TradingPairOf<Runtime>>::key_for(&i);
                if let Some(pair) = Self::pickout::<TradingPair>(&state, &key, Hasher::BLAKE2256)? {
                    let mut info = PairInfo::default();
                    info.id = pair.index;
                    info.assets = String!(pair.base_as_ref());
                    info.currency = String!(pair.quote_as_ref());
                    info.precision = pair.pip_precision;
                    info.online = pair.online;
                    info.unit_precision = pair.tick_precision;

                    let price_key = <xspot::TradingPairInfoOf<Runtime>>::key_for(&i);
                    if let Some(price) = Self::pickout::<(Balance, Balance, BlockNumber)>(
                        &state,
                        &price_key,
                        Hasher::BLAKE2256,
                    )? {
                        info.last_price = price.0;
                        info.aver_price = price.1;
                        info.update_height = price.2;
                    }

                    let handicap_key = <xspot::HandicapOf<Runtime>>::key_for(&i);
                    if let Some(handicap) = Self::pickout::<HandicapInfo<Runtime>>(
                        &state,
                        &handicap_key,
                        Hasher::BLAKE2256,
                    )? {
                        info.buy_one = handicap.highest_bid;
                        info.sell_one = handicap.lowest_offer;

                        if !handicap.lowest_offer.is_zero() {
                            info.maximum_bid = handicap.lowest_offer + pair.fluctuation();
                        }
                        info.minimum_offer = if handicap.highest_bid > pair.fluctuation() {
                            handicap.highest_bid - pair.fluctuation()
                        } else {
                            pair.tick()
                        }
                    }

                    pairs.push(info);
                }
            }
        }

        Ok(Some(pairs))
    }

    fn quotations(
        &self,
        pair_index: TradingPairIndex,
        piece: u32,
        hash: Option<<Block as BlockT>::Hash>,
    ) -> Result<Option<QuotationsList>> {
        if piece < 1 || piece > 10 {
            return Err(ErrorKind::QuotationsPieceErr.into());
        }

        let mut quotationslist = QuotationsList::default();
        quotationslist.id = pair_index;
        quotationslist.piece = piece;

        let state = self.state_at(hash)?;

        let sum_of_quotations = |orders: Vec<(AccountId, OrderIndex)>| {
            orders
                .iter()
                .map(|q| {
                    let order_key = <xspot::OrderInfoOf<Runtime>>::key_for(q);
                    Self::pickout::<OrderInfo<Runtime>>(&state, &order_key, Hasher::BLAKE2256)
                        .unwrap()
                })
                .map(|order| {
                    let order = order.unwrap();
                    order
                        .amount()
                        .checked_sub(order.already_filled)
                        .unwrap_or_default()
                })
                .sum::<Balance>()
        };

        let push_sum_quotations_at =
            |price: Balance, quotations_info: &mut Vec<(Balance, Balance)>| -> Result<()> {
                let quotations_key = <xspot::QuotationsOf<Runtime>>::key_for(&(pair_index, price));

                if let Some(orders) = Self::pickout::<Vec<(AccountId, OrderIndex)>>(
                    &state,
                    &quotations_key,
                    Hasher::BLAKE2256,
                )? {
                    if !orders.is_empty() {
                        quotations_info.push((price, sum_of_quotations(orders)));
                    }
                };

                Ok(())
            };

        quotationslist.sell = Vec::new();
        quotationslist.buy = Vec::new();

        let pair_key = <xspot::TradingPairOf<Runtime>>::key_for(&pair_index);
        if let Some(pair) = Self::pickout::<TradingPair>(&state, &pair_key, Hasher::BLAKE2256)? {
            let tick = pair.tick();

            let handicap_key = <xspot::HandicapOf<Runtime>>::key_for(&pair_index);
            if let Some(handicap) =
                Self::pickout::<HandicapInfo<Runtime>>(&state, &handicap_key, Hasher::BLAKE2256)?
            {
                let (lowest_offer, highest_bid) = (handicap.lowest_offer, handicap.highest_bid);

                let maximum_bid = if lowest_offer.is_zero() {
                    0
                } else {
                    lowest_offer + pair.fluctuation()
                };

                let minimum_offer = if highest_bid > pair.fluctuation() {
                    highest_bid - pair.fluctuation()
                } else {
                    10_u64.pow(pair.tick_precision)
                };

                for price in (lowest_offer..=maximum_bid).step_by(tick as usize) {
                    push_sum_quotations_at(price, &mut quotationslist.sell)?;
                    if quotationslist.buy.len() == piece as usize {
                        break;
                    }
                }

                for price in (minimum_offer..=highest_bid).step_by(tick as usize) {
                    push_sum_quotations_at(price, &mut quotationslist.buy)?;
                    if quotationslist.sell.len() == piece as usize {
                        break;
                    }
                }
            };
        } else {
            return Err(ErrorKind::TradingPairIndexErr.into());
        }

        Ok(Some(quotationslist))
    }

    fn orders(
        &self,
        who: AccountIdForRpc,
        page_index: u32,
        page_size: u32,
        hash: Option<<Block as BlockT>::Hash>,
    ) -> Result<Option<PageData<OrderDetails>>> {
        if page_size > MAX_PAGE_SIZE || page_size < 1 {
            return Err(ErrorKind::PageSizeErr.into());
        }

        let mut orders = Vec::new();
        let mut page_total = 0;

        let state = self.state_at(hash)?;

        let who: AccountId = who.unchecked_into();

        let order_len_key = <xspot::OrderCountOf<Runtime>>::key_for(&who);
        if let Some(len) = Self::pickout::<OrderIndex>(&state, &order_len_key, Hasher::BLAKE2256)? {
            let mut total: u32 = 0;
            for i in (0..len).rev() {
                let order_key = <xspot::OrderInfoOf<Runtime>>::key_for(&(who.clone(), i));
                if let Some(order) =
                    Self::pickout::<OrderInfo<Runtime>>(&state, &order_key, Hasher::BLAKE2256)?
                {
                    if total >= page_index * page_size && total < ((page_index + 1) * page_size) {
                        orders.push(order.into());
                    }
                    total += 1;
                }
            }

            let total_page: u32 = (total + (page_size - 1)) / page_size;

            page_total = total_page;

            if page_index >= total_page && total_page > 0 {
                return Err(ErrorKind::PageIndexErr.into());
            }
        }

        Ok(Some(PageData {
            page_total,
            page_index,
            page_size,
            data: orders,
        }))
    }

    fn address(
        &self,
        who: AccountIdForRpc,
        chain: Chain,
        hash: Option<<Block as BlockT>::Hash>,
    ) -> Result<Option<Vec<String>>> {
        let state = self.state_at(hash)?;

        let who: AccountId = who.unchecked_into();
        match chain {
            Chain::Bitcoin => {
                let key = <xbridge_features::BitcoinCrossChainBinding<Runtime>>::key_for(&who);
                match Self::pickout::<Vec<BitcoinAddress>>(&state, &key, Hasher::BLAKE2256)? {
                    Some(addrs) => {
                        let v = addrs
                            .into_iter()
                            .map(|addr| addr.to_string())
                            .collect::<Vec<_>>();
                        Ok(Some(v))
                    }
                    None => Ok(Some(vec![])),
                }
            }
            Chain::Ethereum => {
                let key = <xbridge_features::EthereumCrossChainBinding<Runtime>>::key_for(&who);
                match Self::pickout::<Vec<EthereumAddress>>(&state, &key, Hasher::BLAKE2256)? {
                    Some(addrs) => {
                        let v = addrs
                            .into_iter()
                            .map(|addr| {
                                let addr: H160 = addr.into();
                                format!("{:?}", addr)
                            })
                            .collect::<Vec<_>>();
                        Ok(Some(v))
                    }
                    None => Ok(Some(vec![])),
                }
            }
            _ => Err(ErrorKind::RuntimeErr(b"not support for this chain".to_vec()).into()),
        }
    }

    fn trustee_session_info_for(
        &self,
        chain: Chain,
        number: Option<u32>,
        hash: Option<<Block as BlockT>::Hash>,
    ) -> Result<Option<Value>> {
        if let Some((number, info)) =
            self.trustee_session_info_for(self.block_id_by_hash(hash)?, chain, number)?
        {
            return Ok(parse_trustee_session_info(chain, number, info));
        } else {
            return Ok(None);
        }
    }

    fn trustee_info_for_accountid(
        &self,
        who: AccountIdForRpc,
        hash: Option<<Block as BlockT>::Hash>,
    ) -> Result<Option<Value>> {
        let who: AccountId = who.unchecked_into();
        let props_info = self.trustee_props_for(self.block_id_by_hash(hash)?, who)?;
        Ok(parse_trustee_props(props_info))
    }

    fn fee(
        &self,
        call_params: String,
        tx_length: u64,
        hash: Option<<Block as BlockT>::Hash>,
    ) -> Result<Option<u64>> {
        if !call_params.starts_with("0x") {
            return Err(ErrorKind::BinanryStartErr.into());
        }
        let call_params: Vec<u8> = if let Ok(hex_call) = call_params[2..].from_hex() {
            hex_call
        } else {
            return Err(ErrorKind::HexDecodeErr.into());
        };
        let call: Call = if let Some(call) = Decode::decode(&mut call_params.as_slice()) {
            call
        } else {
            return Err(ErrorKind::DecodeErr.into());
        };

        let transaction_fee =
            self.transaction_fee(self.block_id_by_hash(hash)?, call.encode(), tx_length)?;

        Ok(transaction_fee)
    }

    fn fee_weight_map(
        &self,
        hash: Option<<Block as BlockT>::Hash>,
    ) -> Result<BTreeMap<String, u64>> {
        self.client
            .runtime_api()
            .fee_weight_map(&self.block_id_by_hash(hash)?)
            .map(|m| {
                m.into_iter()
                    .map(|(k, v)| (String::from_utf8_lossy(&k).into_owned(), v))
                    .collect()
            })
            .map_err(Into::into)
    }

    fn withdraw_tx(
        &self,
        chain: Chain,
        hash: Option<<Block as BlockT>::Hash>,
    ) -> Result<Option<WithdrawTxInfo>> {
        let state = self.state_at(hash)?;
        match chain {
            Chain::Bitcoin => {
                let key = <xbitcoin::CurrentWithdrawalProposal<Runtime>>::key();
                Self::pickout::<xbitcoin::WithdrawalProposal<AccountId>>(
                    &state,
                    &key,
                    Hasher::TWOX128,
                )
                .map(|option_data| {
                    option_data.map(|proposal| WithdrawTxInfo::from_bitcoin_proposal(proposal))
                })
            }
            _ => Ok(None),
        }
    }

    fn mock_bitcoin_new_trustees(
        &self,
        candidates: Vec<AccountIdForRpc>,
        hash: Option<<Block as BlockT>::Hash>,
    ) -> Result<Option<Value>> {
        let candidates: Vec<AccountId> = candidates
            .into_iter()
            .map(|a| a.unchecked_into())
            .collect::<Vec<_>>();

        let runtime_result: result::Result<GenericAllSessionInfo<AccountId>, Vec<u8>> = self
            .client
            .runtime_api()
            .mock_new_trustees(&self.block_id_by_hash(hash)?, Chain::Bitcoin, candidates)?;

        runtime_result
            .map(|all_session_info| parse_trustee_session_info(Chain::Bitcoin, 0, all_session_info))
            .map_err(|e| ErrorKind::RuntimeErr(e).into())
    }

    fn particular_accounts(&self, hash: Option<<Block as BlockT>::Hash>) -> Result<Option<Value>> {
        let state = self.state_at(hash)?;

        // team addr
        let key = xaccounts::TeamAccount::<Runtime>::key();
        let team_account = Self::pickout::<AccountId>(&state, &key, Hasher::TWOX128)?;

        let key = xaccounts::CouncilAccount::<Runtime>::key();
        let council_account = Self::pickout::<AccountId>(&state, &key, Hasher::TWOX128)?;

        let mut map = BTreeMap::new();
        for chain in Chain::iterator() {
            let key = xbridge_features::TrusteeMultiSigAddr::<Runtime>::key_for(chain);
            let addr = Self::pickout::<AccountId>(&state, &key, Hasher::BLAKE2256)?;
            if let Some(a) = addr {
                map.insert(chain, a);
            }
        }

        Ok(Some(json!(
        {
            "teamAccount": team_account,
            "councilAccount": council_account,
            "trusteesAccount": map
        }
        )))
    }
}

fn into_pagedata<T>(src: Vec<T>, page_index: u32, page_size: u32) -> Result<Option<PageData<T>>> {
    if page_size == 0 {
        return Err(ErrorKind::PageSizeErr.into());
    }

    let page_total = (src.len() as u32 + (page_size - 1)) / page_size;
    if page_index >= page_total && page_total > 0 {
        return Err(ErrorKind::PageIndexErr.into());
    }

    let mut list = vec![];
    for (index, item) in src.into_iter().enumerate() {
        let index = index as u32;
        if index >= page_index * page_size && index < ((page_index + 1) * page_size) {
            list.push(item);
        }
    }

    Ok(Some(PageData {
        page_total,
        page_index,
        page_size,
        data: list,
    }))
}